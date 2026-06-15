//! The inbound wire payloads the extension sends, and their lowering into domain types.
//!
//! The host is an edge adapter — it is the one place that knows the Thunderbird wire shape,
//! so the `serde` structs live here, not in the core. Each payload deserializes leniently
//! (the codec already proved the frame is JSON) and lowers into a domain value the core
//! use-cases understand: a `classify_message` payload becomes a [`MessageData`], a
//! `record_user_action` payload becomes a routed [`RecordedEvent`], a `draft_reply` payload
//! becomes a [`ReplyDraftRequest`]. Nothing here reaches a backend.

use serde::Deserialize;

use mailmate_common::ids::{
    AccountId, FolderId, MessageId, PipelineItemId, ThreadId, WorkflowDefId, WorkflowInstanceId,
};
use mailmate_common::mail::{Attachment, MessageData, MessageHeaders};
use mailmate_common::pipeline::{ItemType, NewPipelineItem};
use mailmate_common::reply::ReplyDraftRequest;
use mailmate_common::time::Timestamp;
use mailmate_common::workflow::{ExitEvent, ReviewResolution};

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
    /// The thread it concerns (a `reply_received` event keys off this to exit follow-ups).
    #[serde(default)]
    pub thread_id: Option<String>,
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

/// The `enroll_pipeline_item` control request: tag a quote/proposal → create a
/// `pipeline_item` and arm a workflow on it. The item is always user-enrolled.
#[derive(Clone, Debug, Deserialize)]
pub struct EnrollPipelineItemPayload {
    /// Owning account.
    pub account_id: String,
    /// The outbound quote/proposal thread.
    pub thread_id: String,
    /// The sent quote message (Thunderbird id), when known.
    #[serde(default)]
    pub anchor_thunderbird_message_id: Option<String>,
    /// Who we follow up with.
    pub counterparty_email: String,
    /// The counterparty domain.
    #[serde(default)]
    pub counterparty_domain: String,
    /// A short human title.
    #[serde(default)]
    pub title: String,
    /// `quote` or `proposal` (defaults to `quote`).
    #[serde(default)]
    pub item_type: Option<String>,
    /// A display-only amount hint.
    #[serde(default)]
    pub amount_hint: Option<String>,
    /// The workflow definition to arm (`wfd_…`).
    pub workflow_id: String,
}

impl EnrollPipelineItemPayload {
    /// Lower the payload into a [`NewPipelineItem`] (the host stamps id/stage/timestamps).
    #[must_use]
    pub fn into_new_item(&self) -> NewPipelineItem {
        NewPipelineItem {
            account_id: self.account_id.clone(),
            thread_id: ThreadId::from(self.thread_id.as_str()),
            anchor_message_id: self
                .anchor_thunderbird_message_id
                .as_deref()
                .map(internal_message_id),
            counterparty_email: self.counterparty_email.clone(),
            counterparty_domain: self.counterparty_domain.clone(),
            title: self.title.clone(),
            item_type: self
                .item_type
                .as_deref()
                .and_then(ItemType::from_db_str)
                .unwrap_or(ItemType::Quote),
            amount_hint: self.amount_hint.clone(),
        }
    }

    /// The workflow definition id to arm on.
    #[must_use]
    pub fn workflow_def_id(&self) -> WorkflowDefId {
        WorkflowDefId::from(self.workflow_id.as_str())
    }
}

/// The `update_pipeline_stage` control request: mark won/lost (closes the sequence).
#[derive(Clone, Debug, Deserialize)]
pub struct UpdatePipelineStagePayload {
    /// The pipeline item to update.
    pub pipeline_item_id: String,
    /// `won` or `lost`.
    pub stage: String,
}

impl UpdatePipelineStagePayload {
    /// The item this concerns.
    #[must_use]
    pub fn item_id(&self) -> PipelineItemId {
        PipelineItemId::from(self.pipeline_item_id.as_str())
    }

    /// The exit event the requested stage implies (`won`/`lost`), or `None` if unrecognized.
    #[must_use]
    pub fn exit_event(&self) -> Option<ExitEvent> {
        match self.stage.as_str() {
            "won" => Some(ExitEvent::Won),
            "lost" => Some(ExitEvent::Lost),
            _ => None,
        }
    }
}

/// The `cancel_sequence` control request: stop the workflow on a pipeline item.
#[derive(Clone, Debug, Deserialize)]
pub struct CancelSequencePayload {
    /// The pipeline item whose sequence to cancel.
    pub pipeline_item_id: String,
}

impl CancelSequencePayload {
    /// The item this concerns.
    #[must_use]
    pub fn item_id(&self) -> PipelineItemId {
        PipelineItemId::from(self.pipeline_item_id.as_str())
    }
}

/// The `reschedule_followup` / `snooze` control request: push `next_due_at` out.
#[derive(Clone, Debug, Deserialize)]
pub struct RescheduleFollowupPayload {
    /// The instance to push out.
    pub workflow_instance_id: String,
    /// The new due time.
    pub next_due_at: Timestamp,
}

impl RescheduleFollowupPayload {
    /// The instance this concerns.
    #[must_use]
    pub fn instance_id(&self) -> WorkflowInstanceId {
        WorkflowInstanceId::from(self.workflow_instance_id.as_str())
    }
}

/// The `review_followup` control request: resolve a surfaced draft (`send`/`edit`/`skip`).
#[derive(Clone, Debug, Deserialize)]
pub struct ReviewFollowupPayload {
    /// The instance whose surfaced draft is being resolved.
    pub workflow_instance_id: String,
    /// `send` / `edit` / `skip`.
    pub resolution: String,
}

impl ReviewFollowupPayload {
    /// The instance this concerns.
    #[must_use]
    pub fn instance_id(&self) -> WorkflowInstanceId {
        WorkflowInstanceId::from(self.workflow_instance_id.as_str())
    }

    /// The parsed resolution, or `None` if unrecognized.
    #[must_use]
    pub fn resolution(&self) -> Option<ReviewResolution> {
        ReviewResolution::from_db_str(&self.resolution)
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
        assert!(payload.thread_id.is_none());
    }

    #[test]
    fn enroll_payload_lowers_into_a_new_item_and_workflow() {
        let json = serde_json::json!({
            "account_id": "acct_default",
            "thread_id": "thread_acme",
            "anchor_thunderbird_message_id": "tb_9",
            "counterparty_email": "buyer@acme.test",
            "counterparty_domain": "acme.test",
            "title": "Acme quote",
            "item_type": "proposal",
            "workflow_id": "wfd_standard"
        });
        let payload: EnrollPipelineItemPayload = serde_json::from_value(json).unwrap();
        let item = payload.into_new_item();
        assert_eq!(item.item_type, ItemType::Proposal);
        assert_eq!(item.anchor_message_id, Some(internal_message_id("tb_9")));
        assert_eq!(item.thread_id, ThreadId::from("thread_acme"));
        assert_eq!(
            payload.workflow_def_id(),
            WorkflowDefId::from("wfd_standard")
        );
    }

    #[test]
    fn enroll_payload_defaults_item_type_to_quote() {
        let json = serde_json::json!({
            "account_id": "a",
            "thread_id": "t",
            "counterparty_email": "x@y.test",
            "workflow_id": "wfd_1"
        });
        let payload: EnrollPipelineItemPayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload.into_new_item().item_type, ItemType::Quote);
    }

    #[test]
    fn control_payloads_parse_their_discriminators() {
        let stage: UpdatePipelineStagePayload = serde_json::from_value(
            serde_json::json!({ "pipeline_item_id": "pli_1", "stage": "won" }),
        )
        .unwrap();
        assert_eq!(stage.exit_event(), Some(ExitEvent::Won));
        let bad: UpdatePipelineStagePayload = serde_json::from_value(
            serde_json::json!({ "pipeline_item_id": "pli_1", "stage": "paused" }),
        )
        .unwrap();
        assert_eq!(bad.exit_event(), None);

        let review: ReviewFollowupPayload = serde_json::from_value(
            serde_json::json!({ "workflow_instance_id": "wfi_1", "resolution": "skip" }),
        )
        .unwrap();
        assert_eq!(review.resolution(), Some(ReviewResolution::Skip));
        assert_eq!(review.instance_id(), WorkflowInstanceId::from("wfi_1"));
    }
}
