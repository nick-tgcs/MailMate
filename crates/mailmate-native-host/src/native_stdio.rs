//! The native-messaging frame codec and the single-writer output guard.
//!
//! Wire format (Mozilla native messaging): a 32-bit message length in **native byte
//! order**, immediately followed by that many bytes of UTF-8 JSON. The host and the
//! browser run on the same machine, so native byte order is correct and matches what
//! Thunderbird emits.

use std::io::{ErrorKind, Read, Write};
use std::sync::{Mutex, PoisonError};

use mailmate_common::error::TransportError;
use mailmate_common::protocol::Frame;

/// The host→extension frame size limit (1 MiB).
///
/// A response or notification larger than this is refused by [`write_frame`] (and the
/// host substitutes a small structured error instead — see [`crate::dispatch::emit`]),
/// so an oversized frame is never written to the wire.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Defensive cap on a single inbound (extension→host) frame.
///
/// The wire format permits larger extension→host messages, but MailMate never needs
/// them, so the codec refuses an absurd declared length rather than allocate it — a
/// cheap guard against a hostile or buggy peer. 16 MiB is far above any real request.
pub const MAX_READ_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Read one frame from `reader`.
///
/// Returns `Ok(None)` on a clean end-of-stream *before* a frame begins (the peer hung
/// up between frames — normal shutdown). Returns:
/// - [`TransportError::FrameTooLarge`] if the declared length exceeds
///   [`MAX_READ_FRAME_BYTES`] (the body is *not* consumed, so the stream is desynced);
/// - [`TransportError::Codec`] if the body is not valid JSON for a [`Frame`] (the body
///   *was* consumed, so the stream stays frame-aligned and the caller may continue);
/// - [`TransportError::Io`] on a truncated length/body or any other read failure.
///
/// # Errors
/// See the variants above.
pub fn read_frame<R: Read>(reader: &mut R) -> Result<Option<Frame>, TransportError> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(TransportError::Io(e.to_string())),
    }

    let len = u32::from_ne_bytes(len_buf) as usize;
    if len > MAX_READ_FRAME_BYTES {
        return Err(TransportError::FrameTooLarge(len));
    }

    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        .map_err(|e| TransportError::Io(e.to_string()))?;

    let frame =
        serde_json::from_slice::<Frame>(&body).map_err(|e| TransportError::Codec(e.to_string()))?;
    Ok(Some(frame))
}

/// Encode `frame` and write it (length prefix + JSON body) to `writer`.
///
/// Enforces the host→extension limit: if the encoded frame exceeds
/// [`MAX_FRAME_BYTES`], returns [`TransportError::FrameTooLarge`] **without writing any
/// bytes**, so a partial or oversized frame never reaches the wire.
///
/// # Errors
/// [`TransportError::Codec`] if the frame cannot be serialized, [`TransportError::FrameTooLarge`]
/// if it is too large, or [`TransportError::Io`] on a write failure.
pub fn write_frame<W: Write>(writer: &mut W, frame: &Frame) -> Result<(), TransportError> {
    let body = serde_json::to_vec(frame).map_err(|e| TransportError::Codec(e.to_string()))?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(TransportError::FrameTooLarge(body.len()));
    }
    // `body.len() <= MAX_FRAME_BYTES` (1 MiB) so the cast cannot truncate.
    let len = body.len() as u32;
    writer
        .write_all(&len.to_ne_bytes())
        .map_err(|e| TransportError::Io(e.to_string()))?;
    writer
        .write_all(&body)
        .map_err(|e| TransportError::Io(e.to_string()))?;
    writer
        .flush()
        .map_err(|e| TransportError::Io(e.to_string()))?;
    Ok(())
}

/// A single-writer guard around an output stream.
///
/// Native messaging multiplexes responses and host-initiated notifications onto one
/// stdout. If two writers interleaved the bytes of two frames, the extension's reader
/// would desync. `FrameWriter` serialises every whole-frame write behind a mutex, so
/// frames are always emitted atomically and in *some* total order.
#[derive(Debug)]
pub struct FrameWriter<W: Write> {
    inner: Mutex<W>,
}

impl<W: Write> FrameWriter<W> {
    /// Wrap a writer.
    #[must_use]
    pub fn new(writer: W) -> Self {
        Self {
            inner: Mutex::new(writer),
        }
    }

    /// Write one frame atomically. Behaves like [`write_frame`] but is safe to call
    /// concurrently from multiple threads. A poisoned lock is recovered rather than
    /// panicking — a prior panic mid-write cannot corrupt a *future* frame because the
    /// codec writes the whole frame under the lock.
    ///
    /// # Errors
    /// Propagates [`write_frame`]'s errors.
    pub fn write(&self, frame: &Frame) -> Result<(), TransportError> {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        write_frame(&mut *guard, frame)
    }

    /// Recover the wrapped writer (used at shutdown and in tests).
    #[must_use]
    pub fn into_inner(self) -> W {
        self.inner
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;

    use mailmate_common::protocol::{ProtocolVersion, ResponseStatus};
    use serde_json::json;

    fn request(id: &str, type_: &str) -> Frame {
        Frame::Request {
            protocol_version: ProtocolVersion::default(),
            request_id: id.to_owned(),
            type_: type_.to_owned(),
            payload: json!({ "nonce": "n" }),
        }
    }

    fn response(id: &str) -> Frame {
        Frame::Response {
            protocol_version: ProtocolVersion::default(),
            request_id: id.to_owned(),
            status: ResponseStatus::Ok,
            payload: Some(json!({ "pong": true })),
            error: None,
        }
    }

    #[test]
    fn round_trips_a_frame_through_the_codec() {
        let frame = request("r1", "ping");
        let mut buf = Vec::new();
        write_frame(&mut buf, &frame).unwrap();

        let mut cursor = Cursor::new(buf);
        let back = read_frame(&mut cursor).unwrap().unwrap();
        assert_eq!(back, frame);
        // Stream is now empty -> clean EOF.
        assert!(read_frame(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn read_returns_none_on_clean_eof() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        assert!(read_frame(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn read_rejects_malformed_json_as_codec_error() {
        let bad = b"{ this is not valid json";
        let mut input = (bad.len() as u32).to_ne_bytes().to_vec();
        input.extend_from_slice(bad);

        let mut cursor = Cursor::new(input);
        let err = read_frame(&mut cursor).unwrap_err();
        assert!(matches!(err, TransportError::Codec(_)), "got {err:?}");
    }

    #[test]
    fn read_rejects_absurd_declared_length() {
        let huge = (MAX_READ_FRAME_BYTES + 1) as u32;
        let input = huge.to_ne_bytes().to_vec();
        let mut cursor = Cursor::new(input);
        let err = read_frame(&mut cursor).unwrap_err();
        assert!(
            matches!(err, TransportError::FrameTooLarge(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn read_errors_on_truncated_body() {
        let mut input = 10u32.to_ne_bytes().to_vec();
        input.extend_from_slice(b"abc"); // promises 10 bytes, supplies 3
        let mut cursor = Cursor::new(input);
        let err = read_frame(&mut cursor).unwrap_err();
        assert!(matches!(err, TransportError::Io(_)), "got {err:?}");
    }

    #[test]
    fn write_frame_refuses_oversize_and_writes_nothing() {
        let blob = "x".repeat(MAX_FRAME_BYTES); // body alone exceeds the 1 MiB cap once framed
        let frame = Frame::Notification {
            protocol_version: ProtocolVersion::default(),
            notification_id: "n1".to_owned(),
            type_: "classification_ready".to_owned(),
            payload: json!({ "blob": blob }),
        };
        let mut buf = Vec::new();
        let err = write_frame(&mut buf, &frame).unwrap_err();
        assert!(
            matches!(err, TransportError::FrameTooLarge(_)),
            "got {err:?}"
        );
        assert!(buf.is_empty(), "no bytes should have been written");
    }

    #[test]
    fn concurrent_writes_never_interleave_frames() {
        const N: usize = 50;
        let writer = Arc::new(FrameWriter::new(Vec::<u8>::new()));

        let mut handles = Vec::new();
        for i in 0..N {
            let w = Arc::clone(&writer);
            handles.push(std::thread::spawn(move || {
                let frame = if i % 2 == 0 {
                    response(&format!("r{i}"))
                } else {
                    Frame::Notification {
                        protocol_version: ProtocolVersion::default(),
                        notification_id: format!("n{i}"),
                        type_: "classification_ready".to_owned(),
                        payload: json!({ "i": i }),
                    }
                };
                w.write(&frame).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let writer = Arc::try_unwrap(writer).expect("all writer threads joined");
        let bytes = writer.into_inner();

        // If any two frames had interleaved, a length prefix would not match its body
        // and we would fail to decode exactly N intact frames.
        let mut cursor = Cursor::new(bytes);
        let mut count = 0;
        while read_frame(&mut cursor).unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, N, "every frame must be whole and recoverable");
    }
}
