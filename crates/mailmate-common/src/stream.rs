//! Stream aliases for the event/transport ports.
//!
//! Kept here (next to the types they carry) so the port traits read cleanly. The
//! streams are `Send` because the host moves them across async tasks; `'static`
//! because they outlive any single call.

use crate::error::TransportError;
use crate::protocol::Frame;

/// A boxed, `Send` stream.
pub type BoxStream<'a, T> = std::pin::Pin<Box<dyn futures::Stream<Item = T> + Send + 'a>>;

/// Stream of mail-client events (`MailClient::events`).
pub type EventStream<T> = BoxStream<'static, T>;

/// Stream of inbound native-messaging frames (`Transport::incoming`).
pub type FrameStream = BoxStream<'static, Result<Frame, TransportError>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::MailEvent;
    use futures::StreamExt;

    #[test]
    fn event_stream_can_be_built_and_drained_deterministically() {
        let events = vec![
            MailEvent::ReadChanged {
                client_message_id: "1".to_owned(),
                read: true,
                occurred_at: crate::time::Timestamp::now(),
            },
            MailEvent::Tagged {
                client_message_id: "1".to_owned(),
                tag: "receipt".to_owned(),
                added: true,
                occurred_at: crate::time::Timestamp::now(),
            },
        ];
        let stream: EventStream<MailEvent> = Box::pin(futures::stream::iter(events.clone()));
        let collected: Vec<MailEvent> = futures::executor::block_on(stream.collect());
        assert_eq!(collected, events);
    }

    #[test]
    fn frame_stream_carries_results() {
        let stream: FrameStream =
            Box::pin(futures::stream::iter(vec![Err(TransportError::Closed)]));
        let collected: Vec<_> = futures::executor::block_on(stream.collect());
        assert_eq!(collected.len(), 1);
        assert!(collected[0].is_err());
    }
}
