//! Request dispatch and the host run-loop.
//!
//! [`dispatch`] is a pure function from an inbound [`Frame`] to the response [`Frame`]
//! the host should emit. [`run_loop`] drives the codec: read → dispatch → [`emit`],
//! until the peer hangs up. [`emit`] enforces the oversize-frame guard (a too-large
//! response becomes a small structured error, never a partial write).
//!
//! This module serves only the Phase-1 `ping` request plus the protocol-level errors
//! (version / kind / unknown-type); its `ok_response` / `error_response` / `emit` helpers are
//! the shared frame builders. The Phase-10 request types (`classify_message`, `new_mail`,
//! `draft_reply`, `record_user_action`) are NOT new arms here — they are routed by
//! [`crate::router::HostRouter`], which drives the core use-cases. The shipped binary still runs
//! this ping-only [`run_loop`] until the production composition root (real engines / provider /
//! storage injected into a `Ports`) is wired in Phase 12; see `main.rs`.

use std::io::{Read, Write};

use mailmate_common::error::TransportError;
use mailmate_common::protocol::{Frame, ProtocolError, ProtocolVersion, ResponseStatus};
use serde_json::{json, Value};

use crate::native_stdio::{read_frame, FrameWriter, MAX_FRAME_BYTES};

/// The only wire protocol version this host speaks.
pub const SUPPORTED_PROTOCOL_VERSION: &str = "1.0";

/// Build a successful response frame correlated to `request_id`.
#[must_use]
pub fn ok_response(request_id: String, payload: Value) -> Frame {
    Frame::Response {
        protocol_version: ProtocolVersion::default(),
        request_id,
        status: ResponseStatus::Ok,
        payload: Some(payload),
        error: None,
    }
}

/// Build an error response frame correlated to `request_id`.
#[must_use]
pub fn error_response(
    request_id: String,
    code: &str,
    message: impl Into<String>,
    details: Option<Value>,
) -> Frame {
    Frame::Response {
        protocol_version: ProtocolVersion::default(),
        request_id,
        status: ResponseStatus::Error,
        payload: None,
        error: Some(ProtocolError {
            code: code.to_owned(),
            message: message.into(),
            details,
        }),
    }
}

/// Turn an inbound frame into the response the host should emit.
///
/// Only `request` frames are valid inbound; a `response`/`notification` arriving at the
/// host is a protocol misuse and yields an `unexpected_kind` error. An unsupported
/// `protocol_version` or unknown request `type` yields a structured error that still
/// preserves the correlation id.
#[must_use]
pub fn dispatch(frame: Frame) -> Frame {
    match frame {
        Frame::Request {
            protocol_version,
            request_id,
            type_,
            payload,
        } => {
            if protocol_version.0 != SUPPORTED_PROTOCOL_VERSION {
                let received = protocol_version.0;
                return error_response(
                    request_id,
                    "unsupported_protocol_version",
                    format!("unsupported protocol version {received:?}"),
                    Some(json!({ "supported": SUPPORTED_PROTOCOL_VERSION, "received": received })),
                );
            }
            dispatch_request(&type_, request_id, &payload)
        }
        Frame::Response { request_id, .. } => error_response(
            request_id,
            "unexpected_kind",
            "host received a response frame; it only accepts requests",
            None,
        ),
        Frame::Notification {
            notification_id, ..
        } => error_response(
            notification_id,
            "unexpected_kind",
            "host received a notification frame; it only accepts requests",
            None,
        ),
    }
}

/// Route a (version-checked) request by its `type`.
fn dispatch_request(type_: &str, request_id: String, payload: &Value) -> Frame {
    match type_ {
        "ping" => ok_response(
            request_id,
            json!({
                "pong": true,
                // Echo the caller's nonce so a client can correlate beyond request_id.
                "echo": payload.get("nonce").cloned().unwrap_or(Value::Null),
            }),
        ),
        other => error_response(
            request_id,
            "unknown_request_type",
            format!("unknown request type {other:?}"),
            Some(json!({ "type": other })),
        ),
    }
}

/// Write one outbound frame, applying the oversize-frame guard.
///
/// If the frame is too large for the host→extension limit, a small structured error
/// (`frame_too_large`) is written in its place, so the extension learns the frame was
/// withheld instead of receiving a partial or oversized write.
///
/// # Errors
/// Propagates a non-oversize [`TransportError`] from the underlying writer.
pub fn emit<W: Write>(writer: &FrameWriter<W>, frame: Frame) -> Result<(), TransportError> {
    match writer.write(&frame) {
        Ok(()) => Ok(()),
        Err(TransportError::FrameTooLarge(size)) => writer.write(&oversize_error(&frame, size)),
        Err(other) => Err(other),
    }
}

/// Build the structured replacement for an outbound frame that was too large.
fn oversize_error(original: &Frame, size: usize) -> Frame {
    let id = match original {
        Frame::Request { request_id, .. } | Frame::Response { request_id, .. } => {
            request_id.clone()
        }
        Frame::Notification {
            notification_id, ..
        } => notification_id.clone(),
    };
    error_response(
        id,
        "frame_too_large",
        format!("frame exceeded the {MAX_FRAME_BYTES}-byte native-messaging limit ({size} bytes); withheld"),
        None,
    )
}

/// Run the host loop: read a frame, dispatch it, emit the response — until EOF.
///
/// A malformed (non-JSON) inbound frame is answered with a `malformed_frame` error and
/// the loop continues, because the codec consumed the whole framed body and the stream
/// stays aligned. A fatal stream condition (I/O failure, or an inbound frame whose
/// declared length desynced the stream) ends the loop with that error.
///
/// # Errors
/// Returns the fatal [`TransportError`] that ended the loop, if any.
pub fn run_loop<R: Read, W: Write>(
    reader: &mut R,
    writer: &FrameWriter<W>,
) -> Result<(), TransportError> {
    loop {
        match read_frame(reader) {
            Ok(None) => return Ok(()),
            Ok(Some(frame)) => emit(writer, dispatch(frame))?,
            Err(TransportError::Codec(message)) => {
                emit(
                    writer,
                    error_response(String::new(), "malformed_frame", message, None),
                )?;
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    use mailmate_common::protocol::ProtocolVersion;

    use crate::native_stdio::{read_frame, write_frame};

    fn req(id: &str, version: &str, type_: &str, payload: Value) -> Frame {
        Frame::Request {
            protocol_version: ProtocolVersion(version.to_owned()),
            request_id: id.to_owned(),
            type_: type_.to_owned(),
            payload,
        }
    }

    fn as_error(frame: &Frame) -> &ProtocolError {
        match frame {
            Frame::Response {
                status: ResponseStatus::Error,
                error: Some(e),
                ..
            } => e,
            other => panic!("expected an error response, got {other:?}"),
        }
    }

    #[test]
    fn ping_returns_pong_echoes_nonce_and_preserves_request_id() {
        let frame = dispatch(req("req_7", "1.0", "ping", json!({ "nonce": "abc" })));
        match frame {
            Frame::Response {
                request_id,
                status: ResponseStatus::Ok,
                payload: Some(payload),
                ..
            } => {
                assert_eq!(request_id, "req_7");
                assert_eq!(payload["pong"], json!(true));
                assert_eq!(payload["echo"], json!("abc"));
            }
            other => panic!("expected ok response, got {other:?}"),
        }
    }

    #[test]
    fn unknown_request_type_is_rejected() {
        let frame = dispatch(req("req_1", "1.0", "do_something_unknown", json!({})));
        let err = as_error(&frame);
        assert_eq!(err.code, "unknown_request_type");
        // Correlation id preserved.
        match &frame {
            Frame::Response { request_id, .. } => assert_eq!(request_id, "req_1"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn unsupported_protocol_version_is_rejected() {
        let frame = dispatch(req("req_1", "9.9", "ping", json!({})));
        let err = as_error(&frame);
        assert_eq!(err.code, "unsupported_protocol_version");
        assert_eq!(err.details.as_ref().unwrap()["received"], json!("9.9"));
    }

    #[test]
    fn response_frame_inbound_is_unexpected_kind() {
        let inbound = ok_response("req_5".to_owned(), json!({}));
        let frame = dispatch(inbound);
        assert_eq!(as_error(&frame).code, "unexpected_kind");
    }

    #[test]
    fn notification_frame_inbound_is_unexpected_kind() {
        let inbound = Frame::Notification {
            protocol_version: ProtocolVersion::default(),
            notification_id: "ntf_1".to_owned(),
            type_: "classification_ready".to_owned(),
            payload: json!({}),
        };
        let frame = dispatch(inbound);
        assert_eq!(as_error(&frame).code, "unexpected_kind");
    }

    #[test]
    fn emit_writes_a_normal_frame_through() {
        let writer = FrameWriter::new(Vec::<u8>::new());
        emit(&writer, ok_response("r1".to_owned(), json!({ "ok": 1 }))).unwrap();
        let mut cursor = Cursor::new(writer.into_inner());
        let back = read_frame(&mut cursor).unwrap().unwrap();
        match back {
            Frame::Response { request_id, .. } => assert_eq!(request_id, "r1"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn emit_substitutes_a_structured_error_for_an_oversize_frame() {
        let blob = "x".repeat(MAX_FRAME_BYTES);
        let oversize = Frame::Notification {
            protocol_version: ProtocolVersion::default(),
            notification_id: "ntf_big".to_owned(),
            type_: "classification_ready".to_owned(),
            payload: json!({ "blob": blob }),
        };
        let writer = FrameWriter::new(Vec::<u8>::new());
        emit(&writer, oversize).unwrap();

        let bytes = writer.into_inner();
        let mut cursor = Cursor::new(bytes.clone());
        let first = read_frame(&mut cursor).unwrap().unwrap();
        let err = as_error(&first);
        assert_eq!(err.code, "frame_too_large");
        // Exactly one (small) frame was written...
        assert!(read_frame(&mut cursor).unwrap().is_none());
        // ...and the oversize payload never reached the wire.
        assert!(
            bytes.len() < MAX_FRAME_BYTES,
            "oversize bytes were withheld"
        );
    }

    #[test]
    fn run_loop_answers_ping_then_handles_malformed_then_eof() {
        // Build an input stream: [valid ping][malformed frame][valid ping].
        let mut input = Vec::new();
        write_frame(&mut input, &req("a", "1.0", "ping", json!({ "nonce": 1 }))).unwrap();

        let bad = b"{not json";
        input.extend_from_slice(&(bad.len() as u32).to_ne_bytes());
        input.extend_from_slice(bad);

        write_frame(&mut input, &req("b", "1.0", "ping", json!({ "nonce": 2 }))).unwrap();

        let mut reader = Cursor::new(input);
        let writer = FrameWriter::new(Vec::<u8>::new());
        run_loop(&mut reader, &writer).unwrap();

        // Decode the three response frames.
        let mut out = Cursor::new(writer.into_inner());
        let f1 = read_frame(&mut out).unwrap().unwrap();
        let f2 = read_frame(&mut out).unwrap().unwrap();
        let f3 = read_frame(&mut out).unwrap().unwrap();
        assert!(read_frame(&mut out).unwrap().is_none());

        match f1 {
            Frame::Response {
                request_id, status, ..
            } => {
                assert_eq!(request_id, "a");
                assert_eq!(status, ResponseStatus::Ok);
            }
            other => panic!("got {other:?}"),
        }
        assert_eq!(as_error(&f2).code, "malformed_frame");
        match f3 {
            Frame::Response {
                request_id, status, ..
            } => {
                assert_eq!(request_id, "b");
                assert_eq!(status, ResponseStatus::Ok);
            }
            other => panic!("got {other:?}"),
        }
    }
}
