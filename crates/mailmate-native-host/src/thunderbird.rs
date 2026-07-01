//! The Thunderbird mail-client adapter and the writer-backed transport.
//!
//! In native messaging the **extension** is the only side that can call Thunderbird's
//! WebExtension APIs, and only the extension may open the connection. So the host commands the
//! client *indirectly*: [`ThunderbirdMailClient::apply`] / `create_draft` emit a `mail_command`
//! frame over the [`Transport`], and the extension executes it and reports the result back via
//! `record_user_action`. The safe actions are idempotent, so they are fire-and-forget
//! commands — no blocking ack runtime is needed, and `never_auto_send_drafts` still holds
//! because the command vocabulary has no "send".
//!
//! `fetch` cannot pull synchronously over this transport, so it reads a session cache the host
//! fills from inbound payloads (the extension already sent the message it read); a cache miss
//! is an honest [`MailError::NotFound`]. `events` is empty here: client events arrive as
//! inbound request frames routed by the host loop, not pulled through this port.
//!
//! [`WriterTransport`] is the production [`Transport`]: it frames outbound frames through the
//! single-writer [`FrameWriter`], applying the same oversize-frame guard the host loop uses.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::json;

use mailmate_common::error::{MailError, TransportError};
use mailmate_common::ids::{DraftId, MessageId};
use mailmate_common::mail::{DraftSpec, FetchScope, MailAction, MailEvent, MessageData};
use mailmate_common::protocol::{Frame, ProtocolVersion};
use mailmate_common::stream::EventStream;
use mailmate_ports::transport::Transport;

use crate::dispatch::emit;
use crate::native_stdio::FrameWriter;

/// The native-messaging command type the host sends the extension to apply an action.
pub const MAIL_COMMAND_TYPE: &str = "mail_command";

/// A [`MailClient`](mailmate_ports::mail_client::MailClient) over native messaging.
pub struct ThunderbirdMailClient {
    out: std::sync::Arc<dyn Transport>,
    cache: Mutex<HashMap<String, MessageData>>,
    command_seq: Mutex<u64>,
}

impl ThunderbirdMailClient {
    /// Build the adapter over the output transport (shared with the host's response channel,
    /// so the single-writer guard serialises commands against responses/notifications).
    #[must_use]
    pub fn new(out: std::sync::Arc<dyn Transport>) -> Self {
        Self {
            out,
            cache: Mutex::new(HashMap::new()),
            command_seq: Mutex::new(0),
        }
    }

    /// Cache a message the host received from the extension, so a later [`fetch`](Self::fetch)
    /// (keyed by the internal [`MessageId`]) can return it without a round trip.
    pub fn cache_message(&self, message: MessageData) {
        if let Some(id) = &message.id {
            self.cache
                .lock()
                .unwrap()
                .insert(id.clone().into_string(), message.clone());
        }
    }

    /// Send a `mail_command` notification with a monotonic command id, mapping a transport
    /// failure into a [`MailError`].
    fn send_command(&self, command: &str, body: serde_json::Value) -> Result<(), MailError> {
        let seq = {
            let mut guard = self.command_seq.lock().unwrap();
            *guard += 1;
            *guard
        };
        let frame = Frame::Notification {
            protocol_version: ProtocolVersion::default(),
            notification_id: format!("cmd_{command}_{seq}"),
            type_: MAIL_COMMAND_TYPE.to_owned(),
            payload: json!({ "command": command, "body": body }),
        };
        self.out
            .send(frame)
            .map_err(|e| MailError::Adapter(e.to_string()))
    }
}

#[async_trait]
impl mailmate_ports::mail_client::MailClient for ThunderbirdMailClient {
    async fn apply(&self, action: MailAction) -> Result<(), MailError> {
        self.send_command("apply", serde_json::to_value(&action).unwrap_or(json!({})))
    }

    async fn create_draft(&self, spec: DraftSpec) -> Result<DraftId, MailError> {
        // The host assigns the correlation id; the extension creates the Thunderbird draft and
        // reports the client-native id back via record_user_action.
        let draft_id = DraftId::fresh();
        self.send_command(
            "create_draft",
            json!({ "draft_id": draft_id, "spec": spec }),
        )?;
        Ok(draft_id)
    }

    async fn fetch(&self, id: MessageId, _scope: FetchScope) -> Result<MessageData, MailError> {
        self.cache
            .lock()
            .unwrap()
            .get(id.as_str())
            .cloned()
            .ok_or(MailError::NotFound(id))
    }

    fn events(&self) -> EventStream<MailEvent> {
        // Client events arrive as inbound request frames routed by the host loop, not pulled
        // through this port — so the pull stream is empty by construction.
        Box::pin(futures::stream::empty())
    }
}

/// A [`Transport`] that frames outbound frames to any [`Write`] via the single-writer
/// [`FrameWriter`]. This is the production stdio transport (`WriterTransport::new(stdout())`);
/// `incoming` is empty because the host loop reads inbound frames directly from its reader.
pub struct WriterTransport<W: Write> {
    writer: FrameWriter<W>,
}

impl<W: Write> WriterTransport<W> {
    /// Wrap a writer.
    #[must_use]
    pub fn new(writer: W) -> Self {
        Self {
            writer: FrameWriter::new(writer),
        }
    }

    /// Recover the wrapped writer (used in tests to inspect the bytes written).
    #[must_use]
    pub fn into_inner(self) -> W {
        self.writer.into_inner()
    }
}

impl<W: Write + Send> Transport for WriterTransport<W> {
    fn send(&self, frame: Frame) -> Result<(), TransportError> {
        // Reuse the host loop's oversize-frame substitution, so an oversized command becomes a
        // small structured error rather than a partial write.
        emit(&self.writer, frame)
    }

    fn incoming(&self) -> mailmate_common::stream::FrameStream {
        Box::pin(futures::stream::empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;

    use futures::executor::block_on;
    use mailmate_common::ids::FolderId;
    use mailmate_common::mail::{MessageData, MessageHeaders};
    use mailmate_ports::mail_client::MailClient;
    use mailmate_test_support::fakes::FakeTransport;

    use crate::native_stdio::read_frame;

    fn message(id: &str) -> MessageData {
        MessageData {
            id: Some(MessageId::from(id)),
            client_message_id: "tb_1".to_owned(),
            account_id: mailmate_common::ids::AccountId::from("acct"),
            folder_id: FolderId::from("inbox"),
            thread_id: None,
            headers: MessageHeaders::default(),
            body_text: None,
            attachments: vec![],
            remote_content_loaded: false,
            sender_seen_count: None,
            sender_in_address_book: None,
        }
    }

    #[test]
    fn apply_emits_a_mail_command_frame() {
        let transport = Arc::new(FakeTransport::new());
        let client = ThunderbirdMailClient::new(transport.clone());
        block_on(client.apply(MailAction::Tag {
            message_id: MessageId::from("msg_1"),
            tag: "receipt".to_owned(),
        }))
        .unwrap();

        let sent = transport.sent_frames();
        assert_eq!(sent.len(), 1);
        match &sent[0] {
            Frame::Notification { type_, payload, .. } => {
                assert_eq!(type_, MAIL_COMMAND_TYPE);
                assert_eq!(payload["command"], "apply");
                assert_eq!(payload["body"]["kind"], "tag");
            }
            other => panic!("expected a notification command, got {other:?}"),
        }
    }

    #[test]
    fn create_draft_returns_a_fresh_id_and_never_sends() {
        let transport = Arc::new(FakeTransport::new());
        let client = ThunderbirdMailClient::new(transport.clone());
        let id = block_on(client.create_draft(DraftSpec {
            subject: "Re: Hi".to_owned(),
            body: "Hello".to_owned(),
            ..DraftSpec::default()
        }))
        .unwrap();
        assert!(id.as_str().starts_with("draft_"));
        let sent = transport.sent_frames();
        assert_eq!(sent.len(), 1);
        // The command vocabulary never contains "send".
        let json = serde_json::to_string(&sent[0]).unwrap();
        assert!(!json.contains("\"send\""));
        match &sent[0] {
            Frame::Notification { payload, .. } => assert_eq!(payload["command"], "create_draft"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn fetch_returns_a_cached_message_or_not_found() {
        let transport = Arc::new(FakeTransport::new());
        let client = ThunderbirdMailClient::new(transport);
        client.cache_message(message("msg_cached"));

        let got =
            block_on(client.fetch(MessageId::from("msg_cached"), FetchScope::Metadata)).unwrap();
        assert_eq!(got.id, Some(MessageId::from("msg_cached")));

        let err = block_on(client.fetch(MessageId::from("msg_absent"), FetchScope::Metadata))
            .unwrap_err();
        assert!(matches!(err, MailError::NotFound(_)));
    }

    #[test]
    fn events_pull_is_empty() {
        let transport = Arc::new(FakeTransport::new());
        let client = ThunderbirdMailClient::new(transport);
        let collected: Vec<MailEvent> = block_on(futures::StreamExt::collect(client.events()));
        assert!(collected.is_empty());
    }

    #[test]
    fn writer_transport_frames_to_the_underlying_writer() {
        let transport = WriterTransport::new(Vec::<u8>::new());
        transport
            .send(Frame::Notification {
                protocol_version: ProtocolVersion::default(),
                notification_id: "n1".to_owned(),
                type_: "classification_ready".to_owned(),
                payload: json!({ "ok": true }),
            })
            .unwrap();
        let bytes = transport.into_inner();
        let mut cursor = Cursor::new(bytes);
        let frame = read_frame(&mut cursor).unwrap().unwrap();
        match frame {
            Frame::Notification {
                notification_id, ..
            } => assert_eq!(notification_id, "n1"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn writer_transport_incoming_is_empty() {
        let transport = WriterTransport::new(Vec::<u8>::new());
        let frames: Vec<_> = block_on(futures::StreamExt::collect::<Vec<_>>(transport.incoming()));
        assert!(frames.is_empty());
    }
}
