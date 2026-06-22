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
    /// How many prior messages from this sender the extension counted (bounded, best-effort).
    #[serde(default)]
    pub sender_seen_count: Option<u32>,
    /// Whether the sender is in the user's address book (a strong not-spam signal).
    #[serde(default)]
    pub sender_in_address_book: Option<bool>,
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
            sender_seen_count: self.sender_seen_count,
            sender_in_address_book: self.sender_in_address_book,
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

/// The `regenerate_draft` payload: the same reply context as [`DraftReplyPayload`] (flattened in
/// on the wire), plus the user's steer — free-text "make it …" guidance and/or the quick-steer
/// chips they tapped. Both are folded into the model guidance, so regeneration reuses the whole
/// `draft_reply` path unchanged: only the instruction differs.
#[derive(Clone, Debug, Deserialize)]
pub struct RegenerateDraftPayload {
    /// The original reply context (thread/messages/subject/counterparty/excerpt/forbidden list).
    #[serde(flatten)]
    pub base: DraftReplyPayload,
    /// Free-text steer from the Adjust box ("warmer, and ask for the PO number").
    #[serde(default)]
    pub steer: Option<String>,
    /// Quick-steer chip labels the user tapped ("Shorter", "Warmer", "More formal", …).
    #[serde(default)]
    pub adjustments: Vec<String>,
}

impl RegenerateDraftPayload {
    /// Lower into a [`ReplyDraftRequest`], folding the chips and free-text steer into the user
    /// instruction (after any instruction the base payload already carried). Each chip becomes a
    /// "Make it <chip>." clause; blank steer/chips are dropped so the guidance never carries noise.
    #[must_use]
    pub fn into_request(self) -> ReplyDraftRequest {
        let mut request = self.base.into_request();
        let mut parts: Vec<String> = Vec::new();
        if let Some(existing) = request.user_instruction.take() {
            let existing = existing.trim();
            if !existing.is_empty() {
                parts.push(existing.to_owned());
            }
        }
        for chip in self.adjustments {
            let chip = chip.trim();
            if !chip.is_empty() {
                parts.push(format!("Make it {}.", chip.to_lowercase()));
            }
        }
        if let Some(steer) = self.steer {
            let steer = steer.trim();
            if !steer.is_empty() {
                parts.push(steer.to_owned());
            }
        }
        request.user_instruction = if parts.is_empty() {
            None
        } else {
            Some(parts.join(" "))
        };
        request
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
    /// The tag added/removed by a `tag_changed` event.
    #[serde(default)]
    pub tag: Option<String>,
    /// Whether a `tag_changed` event ADDED the tag (`true`) or removed it (`false`).
    #[serde(default)]
    pub added: Option<bool>,
    /// Whether the action was user-initiated (vs MailMate-applied).
    #[serde(default)]
    pub user_initiated: bool,
    /// The sender domain, if the extension knows it (improves correction clustering).
    #[serde(default)]
    pub sender_domain: Option<String>,
    /// The account the corrected message belongs to, if the extension knows it. Folded into a
    /// classification correction's salient features so multi-aspect induction can scope a learned
    /// rule per account. Omitted ⇒ the host falls back to its per-message account cache, then to
    /// account-agnostic (`Global`) induction — degrade, never a false scope.
    #[serde(default)]
    pub account_id: Option<String>,
    /// The folder MailMate had suggested (so a move that agrees is positive reinforcement).
    #[serde(default)]
    pub ai_suggested_folder: Option<String>,
    /// The outcome of an `action_applied` execution result (`ok` / an error string).
    #[serde(default)]
    pub result: Option<String>,
    /// When it happened, if the extension stamped it.
    #[serde(default)]
    pub occurred_at: Option<Timestamp>,
    /// The label the user says is correct (a `classification_corrected` wrong-category fix).
    #[serde(default)]
    pub corrected_label: Option<String>,
    /// The label MailMate had assigned before the correction (so polarity records the
    /// override honestly). Carried by `classification_corrected` and `signal_marked_wrong`.
    #[serde(default)]
    pub prior_label: Option<String>,
    /// The id of the salient signal the user rejected (a `signal_marked_wrong` event) — a
    /// feature key like `auth_fail` or a rule id.
    #[serde(default)]
    pub signal_id: Option<String>,
    /// The kind of action an `action_undone` / `suggestion_dismissed` event concerns
    /// (`move` / `mark_junk` / `tag` / `create_draft`). Selects how an undo is routed.
    #[serde(default)]
    pub action_kind: Option<String>,
    /// The id of the rule that authored an auto-applied action (provenance for an
    /// `action_undone`: undo is the strongest negative signal against the rule that fired).
    #[serde(default)]
    pub rule_id: Option<String>,
    /// For a dismissed/undone suggestion, who authored it (`"model"` or a rule id). Audit
    /// provenance for the ignore/undo-rate signal.
    #[serde(default)]
    pub authored_by: Option<String>,
    /// The reply draft this event concerns (a `draft_diverged` edit-divergence signal carries the
    /// `draft_id` the host minted, so the audit row ties back to the draft that was edited).
    #[serde(default)]
    pub draft_id: Option<String>,
    /// The sender address of a `bounce_received` event's NDR message — the host re-confirms it
    /// looks like an automated bounce agent before exiting the thread's follow-ups.
    #[serde(default)]
    pub sender_email: Option<String>,
    /// The subject line of a `bounce_received` event's NDR message (the other bounce signal the
    /// host re-confirms).
    #[serde(default)]
    pub subject: Option<String>,
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

/// The `record_sent_mail` payload: outbound evidence the extension reports after the user sends a
/// message. The recipients (whose domains the host counts toward a VIP/priority proposal) are the
/// only required content; the subject is optional context.
#[derive(Clone, Debug, Deserialize)]
pub struct SentMailPayload {
    /// The recipient addresses (To/Cc) of the sent message.
    #[serde(default)]
    pub recipients: Vec<String>,
    /// The sent subject, if the extension included it (context only).
    #[serde(default)]
    pub subject: Option<String>,
}

/// The `explain_decision` request: which message's audit timeline to return. The caller may
/// pass the internal `message_id` directly, or the `thunderbird_message_id` the host derives
/// it from (the same derivation `classify`/`record` use), plus an optional cap.
#[derive(Clone, Debug, Deserialize)]
pub struct ExplainDecisionPayload {
    /// The internal message id (`msg_tb_…`), if the caller already has it.
    #[serde(default)]
    pub message_id: Option<String>,
    /// The Thunderbird message id, lowered into the internal id when `message_id` is absent.
    #[serde(default)]
    pub thunderbird_message_id: Option<String>,
    /// Cap the number of (newest-first) timeline entries returned.
    #[serde(default)]
    pub limit: Option<usize>,
}

impl ExplainDecisionPayload {
    /// The internal message id to explain: the explicit id if given, else the one derived
    /// from the Thunderbird id, else `None`.
    #[must_use]
    pub fn message_id(&self) -> Option<MessageId> {
        self.message_id.as_deref().map(MessageId::from).or_else(|| {
            self.thunderbird_message_id
                .as_deref()
                .map(internal_message_id)
        })
    }
}

/// The `list_recent_activity` request: power the dashboard Activity tab's cross-message
/// **global** stream. Unlike `explain_decision` (which is per-message and rejects a request
/// without a `message_id`), this drops the single-message constraint and reads the newest
/// audit entries, optionally narrowed to one of the six event-type *families* the UI's filter
/// chips expose (`classified` / `applied` / `blocked` / `corrected` / `follow_up` / `proposal`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ListRecentActivityPayload {
    /// Cap the number of (newest-first) events returned. Defaults to 50, hard-capped at 500.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Narrow to one event-type family; absent (or `"all"`) returns every family.
    #[serde(default)]
    pub event_type_filter: Option<String>,
}

/// The `list_followups` request: render the Follow-ups pipeline on dashboard open — the tracked
/// deals with their workflow status + next-due step. An optional `status_filter` narrows the
/// view (`active` / `needs_attention` / `won` / `lost` / `all`); `limit` caps the rows.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ListFollowupsPayload {
    /// `active` | `needs_attention` | `won` | `lost` | `all` (default `all`).
    #[serde(default)]
    pub status_filter: Option<String>,
    /// Cap the number of deals returned.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// The `review_rule_proposal` request: a human's accept/reject decision on a pending agent
/// proposal — the Proposals-tab materialization gate. Acceptance materializes the recommended
/// rule **in its recommended status** (shadow/pending-review), never directly `active`: the
/// `decision` distinguishes the user's intent (`accept_for_shadow_mode` / `accept_active` /
/// `reject`), but forcing a rule straight to `active` is a separate promotion the review port
/// does not perform — so both accept variants materialize to the proposal's recommended status.
#[derive(Clone, Debug, Deserialize)]
pub struct ReviewRuleProposalPayload {
    /// The proposal being decided.
    pub proposal_id: String,
    /// `accept_for_shadow_mode` | `accept_active` | `reject`.
    pub decision: String,
    /// A reason chip code for a rejection (the curator learns not to re-propose).
    #[serde(default)]
    pub reason_code: Option<String>,
}

/// The `triage_existing_mail` first-run backfill request: a page of already-present messages the
/// extension swept from `browser.messages.query`. Each is **classified without applying anything**
/// (a dry run, so no mail is moved/marked), and — for messages the user has deliberately filed
/// (i.e. NOT in the inbox) — its current placement is mined as implicit positive evidence to warm
/// the filing clusters. The extension drives the paging, progress, and pause/cancel; the host
/// answers one page at a time.
#[derive(Clone, Debug, Deserialize)]
pub struct TriageExistingMailPayload {
    /// The page of messages to triage.
    pub messages: Vec<ClassifyMessagePayload>,
    /// Whether to mine deliberate folder placements as implicit-positive filing evidence
    /// (defaults to true). The inbox is never mined (an un-triaged arrival is not a placement).
    #[serde(default)]
    pub record_placements: Option<bool>,
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

    #[test]
    fn regenerate_payload_flattens_the_base_context_and_folds_the_steer() {
        let payload: RegenerateDraftPayload = serde_json::from_value(serde_json::json!({
            "subject": "Quote",
            "counterparty": "buyer@acme.test",
            "excerpt": "Can you do better on price?",
            "user_instruction": "Decline the discount.",
            "forbidden_commitments": ["prices"],
            "adjustments": ["Shorter", "Warmer"],
            "steer": "and ask for the PO number"
        }))
        .unwrap();
        // The base context flattened in.
        assert_eq!(payload.base.subject, "Quote");
        assert_eq!(payload.base.forbidden_commitments, vec!["prices".to_owned()]);

        let request = payload.into_request();
        let instruction = request.user_instruction.expect("steer folded into instruction");
        // Original instruction first, then each chip as a clause, then the free-text steer.
        assert!(instruction.starts_with("Decline the discount."), "{instruction}");
        assert!(instruction.contains("Make it shorter."), "{instruction}");
        assert!(instruction.contains("Make it warmer."), "{instruction}");
        assert!(instruction.ends_with("and ask for the PO number"), "{instruction}");
        // The base's forbidden list survives lowering.
        assert_eq!(request.forbidden_commitments, vec!["prices".to_owned()]);
    }

    #[test]
    fn regenerate_payload_with_no_steer_has_no_instruction() {
        let payload: RegenerateDraftPayload = serde_json::from_value(serde_json::json!({
            "subject": "Quote",
            "counterparty": "buyer@acme.test",
            "excerpt": "hi"
        }))
        .unwrap();
        assert!(payload.into_request().user_instruction.is_none());
    }
}
