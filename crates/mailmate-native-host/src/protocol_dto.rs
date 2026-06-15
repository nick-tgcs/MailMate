//! The inbound wire payloads the extension sends, and their lowering into domain types.
//!
//! The host is an edge adapter — it is the one place that knows the Thunderbird wire shape,
//! so the `serde` structs live here, not in the core. Each payload deserializes leniently
//! (the codec already proved the frame is JSON) and lowers into a domain value the core
//! use-cases understand: a `classify_message` payload becomes a [`MessageData`], a
//! `record_user_action` payload becomes a routed [`RecordedEvent`], a `draft_reply` payload
//! becomes a [`ReplyDraftRequest`]. Nothing here reaches a backend.

use serde::Deserialize;

use mailmate_common::ids::{AccountId, FolderId, MessageId, ThreadId};
use mailmate_common::mail::{Attachment, MessageData, MessageHeaders};
use mailmate_common::reply::ReplyDraftRequest;
use mailmate_common::time::Timestamp;

/// Mint the internal [`MessageId`] the pipeline uses for a Thunderbird message.
///
/// The Thunderbird id is client-native; the host derives one stable internal id from it so
/// the classification, the plan, and the audit trail all reference the same message, and so
/// suggested actions carry a target. The mapping is deterministic, so the extension can
/// correlate a response back to the message it asked about by its Thunderbird id (which the
/// response echoes at the top level).
#[must_use]
pub fn internal_message_id(thunderbird_message_id: &str) -> MessageId {
    MessageId::from(format!("msg_tb_{thunderbird_message_id}"))
}

/// The `classify_message` / `read_message` payload: the message the extension read.
#[derive(Clone, Debug, Deserialize)]
pub struct ClassifyMessagePayload {
    /// The client-native message id (Thunderbird's numeric id as text).
    pub thunderbird_message_id: String,
    /// Owning account.
    pub account_id: String,
    /// The folder the message currently lives in.
    pub folder_id: String,
    /// The thread membership, if the extension resolved it.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Parsed headers (the same subset the core reasons over).
    #[serde(default)]
    pub headers: MessageHeaders,
    /// Body text, present only when the extension's retention policy allowed sending it.
    #[serde(default)]
    pub body_text: Option<String>,
    /// Whether the body may be retained (advisory; the host clamps fetch scope to it).
    #[serde(default)]
    pub body_retention_allowed: bool,
    /// Whether remote content was loaded (never loaded for classification by default).
    #[serde(default)]
    pub remote_content_loaded: bool,
    /// Attachment metadata (never content).
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

impl ClassifyMessagePayload {
    /// Lower the wire payload into the domain [`MessageData`] the pipeline classifies.
    #[must_use]
    pub fn into_message_data(self) -> MessageData {
        MessageData {
            id: Some(internal_message_id(&self.thunderbird_message_id)),
            client_message_id: self.thunderbird_message_id,
            account_id: AccountId::from(self.account_id),
            folder_id: FolderId::from(self.folder_id),
            thread_id: self.thread_id.map(ThreadId::from),
            headers: self.headers,
            body_text: self.body_text,
            attachments: self.attachments,
            remote_content_loaded: self.remote_content_loaded,
        }
    }
}

/// The `draft_reply` payload. Beyond the spec's `thread_id` / `message_ids` /
/// `user_instruction` / `forbidden_commitments`, the extension also supplies the bounded
/// `subject` / `counterparty` / `excerpt` it already read, so the host never has to reach
/// back into the mailbox to draft.
#[derive(Clone, Debug, Deserialize)]
pub struct DraftReplyPayload {
    /// The thread being replied to, if known.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// The messages in the thread the reply answers (the last is the in-reply-to).
    #[serde(default)]
    pub message_ids: Vec<String>,
    /// The subject being replied to.
    #[serde(default)]
    pub subject: String,
    /// The counterparty address the reply is addressed to.
    #[serde(default)]
    pub counterparty: String,
    /// A bounded excerpt of the message/thread.
    #[serde(default)]
    pub excerpt: String,
    /// Optional user guidance.
    #[serde(default)]
    pub user_instruction: Option<String>,
    /// Classes of commitment the reply must not make.
    #[serde(default)]
    pub forbidden_commitments: Vec<String>,
}

impl DraftReplyPayload {
    /// Lower the wire payload into a [`ReplyDraftRequest`].
    #[must_use]
    pub fn into_request(self) -> ReplyDraftRequest {
        let in_reply_to = self.message_ids.last().map(|id| internal_message_id(id));
        ReplyDraftRequest {
            thread_id: self.thread_id.map(ThreadId::from),
            in_reply_to,
            subject: self.subject,
            counterparty: self.counterparty,
            excerpt: self.excerpt,
            user_instruction: self.user_instruction,
            forbidden_commitments: self.forbidden_commitments,
        }
    }
}

/// The `record_user_action` payload — the wire carrier for corrections and observed behavior.
#[derive(Clone, Debug, Deserialize)]
pub struct RecordUserActionPayload {
    /// What happened (`message_moved`, `junk_changed`, `read_changed`, `tagged`,
    /// `action_applied`, …). The host routes on this.
    pub event_type: String,
    /// The message it concerns.
    #[serde(default)]
    pub thunderbird_message_id: Option<String>,
    /// The destination folder of a move.
    #[serde(default)]
    pub to_folder_id: Option<String>,
    /// The source folder of a move (provenance only).
    #[serde(default)]
    pub from_folder_id: Option<String>,
    /// The junk state set by a `junk_changed` event.
    #[serde(default)]
    pub junk: Option<bool>,
    /// The read state set by a `read_changed` event.
    #[serde(default)]
    pub read: Option<bool>,
    /// The tag added/removed by a `tagged` event.
    #[serde(default)]
    pub tag: Option<String>,
    /// Whether the action was user-initiated (vs MailMate-applied).
    #[serde(default)]
    pub user_initiated: bool,
    /// The sender domain, if the extension knows it (improves correction clustering).
    #[serde(default)]
    pub sender_domain: Option<String>,
    /// The folder MailMate had suggested (so a move that agrees is positive reinforcement).
    #[serde(default)]
    pub ai_suggested_folder: Option<String>,
    /// The outcome of an `action_applied` execution result (`ok` / an error string).
    #[serde(default)]
    pub result: Option<String>,
    /// When it happened, if the extension stamped it.
    #[serde(default)]
    pub occurred_at: Option<Timestamp>,
}

impl RecordUserActionPayload {
    /// The internal message id this event concerns, if any.
    #[must_use]
    pub fn message_id(&self) -> Option<MessageId> {
        self.thunderbird_message_id
            .as_deref()
            .map(internal_message_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_payload_lowers_into_message_data_with_a_derived_id() {
        let json = serde_json::json!({
            "thunderbird_message_id": "tb_123",
            "account_id": "acct_default",
            "folder_id": "inbox",
            "headers": { "from": "a@x.test", "subject": "Hi" },
            "body_text": "hello",
            "body_retention_allowed": false,
            "attachments": [{ "filename": "x.pdf", "content_type": "application/pdf", "size_bytes": 10 }]
        });
        let payload: ClassifyMessagePayload = serde_json::from_value(json).unwrap();
        let msg = payload.into_message_data();
        assert_eq!(msg.client_message_id, "tb_123");
        assert_eq!(msg.id, Some(MessageId::from("msg_tb_tb_123")));
        assert_eq!(msg.account_id, AccountId::from("acct_default"));
        assert_eq!(msg.headers.from, "a@x.test");
        assert_eq!(msg.attachments.len(), 1);
    }

    #[test]
    fn draft_payload_lowers_with_in_reply_to_as_the_last_message() {
        let json = serde_json::json!({
            "message_ids": ["tb_1", "tb_2"],
            "subject": "Quote",
            "counterparty": "buyer@acme.test",
            "excerpt": "Please advise.",
            "forbidden_commitments": ["prices"]
        });
        let payload: DraftReplyPayload = serde_json::from_value(json).unwrap();
        let req = payload.into_request();
        assert_eq!(req.in_reply_to, Some(internal_message_id("tb_2")));
        assert_eq!(req.forbidden_commitments, vec!["prices".to_owned()]);
    }

    #[test]
    fn record_payload_defaults_are_lenient() {
        let payload: RecordUserActionPayload =
            serde_json::from_value(serde_json::json!({ "event_type": "read_changed" })).unwrap();
        assert_eq!(payload.event_type, "read_changed");
        assert!(payload.message_id().is_none());
        assert!(!payload.user_initiated);
    }
}
