//! The IPC transport port: move native-messaging frames over a wire.

use mailmate_common::error::TransportError;
use mailmate_common::protocol::Frame;
use mailmate_common::stream::FrameStream;

/// A bidirectional frame transport.
///
/// Native-messaging stdio is one adapter; an in-process channel is another (used in
/// tests). The framing/codec lives in the adapter — the port only moves whole frames.
pub trait Transport: Send + Sync {
    /// Send a frame to the peer.
    fn send(&self, frame: Frame) -> Result<(), TransportError>;

    /// The stream of inbound frames (each may fail to decode).
    fn incoming(&self) -> FrameStream;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn Transport) {}
        let _ = takes as fn(&dyn Transport);
    }
}
