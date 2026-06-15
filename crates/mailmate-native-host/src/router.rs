//! The host router: protocol frames in, core use-cases driven, frames out.
//!
//! [`HostRouter`] is the application seam Phase 10 adds — the translator between the
//! Thunderbird wire protocol and the core's use-cases. It owns the [`PlanningService`],
//! [`CorrectionService`], and [`DraftService`] (assembled from the injected [`Ports`]), the
//! [`MailClient`] it applies safe actions through, and the [`AuditRepository`] it records
//! provenance into. Every output frame leaves through one injected [`Transport`], so the
//! single-writer guard keeps responses, notifications, and mail commands from interleaving.
//!
//! The router names only ports and use-cases — no concrete engine, provider, or backend. The
//! Phase-12 composition root ([`crate::runtime`]) injects the real adapters in production; the
//! same router is exercised here against the in-memory fakes, with no change to the code here.
//!
//! ## What each request does
//! - `ping` — liveness echo (the Phase-1 contract, now served here too).
//! - `hello` — richer handshake: host/protocol version, wired capabilities, and the
//!   secret-free drafting/retention posture, so the extension can render Connected /
//!   version-mismatch before sending any real request.
//! - `classify_message` / `read_message` — classify a *selected* message and return the
//!   guarded plan as suggestions, each tagged with its `apply_state` (`suggest` here — a manual
//!   classify applies nothing; the user is in the loop).
//! - `new_mail` — classify a *background* arrival, **apply** the policy-allowed low-risk
//!   actions through the [`MailClient`], and push a `classification_ready` notification that
//!   surfaces the review-required ones. This is "apply safe actions returned by the host".
//! - `draft_reply` — generate an advisory, review-required draft.
//! - `record_user_action` — route a correction to its per-task feedback table, or a pure
//!   provenance/execution-result fact to the audit log — never to both. Beyond the original
//!   `junk_changed` / `message_moved` corrections, three learning discriminants:
//!   `classification_corrected` (a wrong-category fix → classification feedback),
//!   `action_undone` (an Undo of an auto-applied action → negative evidence against the rule
//!   that fired — a reverted move/junk reuses the filing/not-spam corrections; a reverted
//!   tag/draft has no feedback table and is audited), and `suggestion_dismissed` (the
//!   ignore-rate signal — audited, never fabricated into a feedback row it has no chosen
//!   label/folder for).

use std::io::Read;
use std::sync::Arc;

use futures::executor::block_on;
use serde_json::{json, Value};

use mailmate_common::action::{GuardedActionPlan, PlannedAction};
use mailmate_common::actor::Actor;
use mailmate_common::audit::{event_type, AuditEntry, AuditQuery};
use mailmate_common::correction::UserCorrection;
use mailmate_common::curator::ReviewDecision;
use mailmate_common::error::TransportError;
use mailmate_common::features::FeatureVector;
use mailmate_common::ids::{FolderId, ProposalId, ThreadId};
use mailmate_common::mail::MailAction;
use mailmate_common::policy::TriggerKind;
use mailmate_common::proposal::ProposalStatus;
use mailmate_common::protocol::{Frame, ProtocolVersion};
use mailmate_common::workflow::ExitEvent;
use mailmate_core::{CorrectionContext, CorrectionService, DraftService, PlanningService, Ports};
use mailmate_ports::clock::Clock;
use mailmate_ports::exit_detector::ExitDetector;
use mailmate_ports::follow_up_scheduler::FollowUpScheduler;
use mailmate_ports::mail_client::MailClient;
use mailmate_ports::proposal_review::ProposalReview;
use mailmate_ports::storage::pipeline_items::PipelineItemRepository;
use mailmate_ports::storage::{AuditRepository, ProposalRepository};
use mailmate_ports::transport::Transport;
use mailmate_ports::workflow_engine::WorkflowEngine;

use crate::config::SettingsSnapshot;
use crate::convert::{
    classification_ready_payload, classify_response_payload, followup_draft_ready_payload,
    followup_needs_attention_payload,
};
use crate::dispatch::{error_response, ok_response, SUPPORTED_PROTOCOL_VERSION};
use crate::native_stdio::read_frame;
use crate::protocol_dto::{
    CancelSequencePayload, ClassifyMessagePayload, DraftReplyPayload, EnrollPipelineItemPayload,
    ExplainDecisionPayload, ListRecentActivityPayload, RecordUserActionPayload,
    RescheduleFollowupPayload, ReviewFollowupPayload, ReviewRuleProposalPayload,
    UpdatePipelineStagePayload,
};

/// The follow-up engine + repository ports the router needs to serve the sales-pipeline
/// control requests and drain due steps. They live outside the core's [`Ports`] (the core
/// never schedules), so the host injects them as a bundle. A router built without them
/// answers the follow-up request types with `followups_not_configured`.
#[derive(Clone)]
pub struct FollowUpSuite {
    /// The pipeline-item store (enroll / reply-exit lookups).
    pub pipeline_items: Arc<dyn PipelineItemRepository>,
    /// Arms instances, reschedules, resolves reviews, detects conflicts.
    pub workflow_engine: Arc<dyn WorkflowEngine>,
    /// The catch-up-on-launch drain.
    pub scheduler: Arc<dyn FollowUpScheduler>,
    /// Reply / won / lost / cancel exit handling.
    pub exit_detector: Arc<dyn ExitDetector>,
}

/// The read-only management surface the host exposes for the review/explanation UI: the
/// proposal store (pending reviews) and a secret-free settings snapshot. Like the follow-up
/// suite, it lives outside the core's [`Ports`] and is injected by the composition root; a
/// router without it answers the admin request types with `admin_not_configured`.
#[derive(Clone)]
pub struct AdminSuite {
    /// The agent-proposal store (for the pending-review queue).
    pub proposals: Arc<dyn ProposalRepository>,
    /// The effective, secret-free settings.
    pub settings: SettingsSnapshot,
}

/// Routes protocol frames into the core and emits the resulting frames.
#[derive(Clone)]
pub struct HostRouter {
    planning: PlanningService,
    correction: CorrectionService,
    draft: DraftService,
    mail_client: Arc<dyn MailClient>,
    audit: Arc<dyn AuditRepository>,
    clock: Arc<dyn Clock>,
    proposal_review: Arc<dyn ProposalReview>,
    out: Arc<dyn Transport>,
    followups: Option<FollowUpSuite>,
    admin: Option<AdminSuite>,
}

impl HostRouter {
    /// Assemble the router from the core [`Ports`], the audit sink, and the output transport.
    /// The follow-up control requests are inert until [`with_followups`](Self::with_followups)
    /// supplies the workflow engine/scheduler/exit-detector + pipeline-item store.
    #[must_use]
    pub fn from_ports(
        ports: &Ports,
        audit: Arc<dyn AuditRepository>,
        out: Arc<dyn Transport>,
    ) -> Self {
        Self {
            planning: PlanningService::from_ports(ports),
            correction: CorrectionService::from_ports(ports),
            draft: DraftService::from_ports(ports),
            mail_client: ports.mail_client.clone(),
            audit,
            clock: ports.clock.clone(),
            proposal_review: ports.proposal_review.clone(),
            out,
            followups: None,
            admin: None,
        }
    }

    /// Wire the follow-up engine suite, enabling the sales-pipeline control requests and the
    /// [`drain_followups`](Self::drain_followups) sweep.
    #[must_use]
    pub fn with_followups(mut self, followups: FollowUpSuite) -> Self {
        self.followups = Some(followups);
        self
    }

    /// Wire the management surface, enabling `list_pending_reviews` and `get_settings`.
    #[must_use]
    pub fn with_admin(mut self, admin: AdminSuite) -> Self {
        self.admin = Some(admin);
        self
    }

    /// Drive the host loop synchronously: read a frame, [`handle`](Self::handle) it (blocking
    /// on the async use-cases — native messaging is a serial request/response stream), and
    /// continue until the peer hangs up. A malformed inbound frame is answered with a
    /// `malformed_frame` error and the loop continues (the codec stays frame-aligned).
    ///
    /// # Errors
    /// Returns the fatal [`TransportError`] that ended the loop (an I/O failure or a desynced
    /// stream), if any.
    pub fn serve_blocking<R: Read>(&self, reader: &mut R) -> Result<(), TransportError> {
        loop {
            match read_frame(reader) {
                Ok(None) => return Ok(()),
                Ok(Some(frame)) => block_on(self.handle(frame))?,
                Err(TransportError::Codec(message)) => {
                    self.send(error_response(
                        String::new(),
                        "malformed_frame",
                        message,
                        None,
                    ))?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Handle one inbound frame, emitting the response and/or notification(s) it produces.
    ///
    /// # Errors
    /// Propagates a [`TransportError`] from the output transport. A *handler* failure (bad
    /// payload, a use-case error) is turned into an error **response frame**, not an `Err` —
    /// the channel stays alive.
    pub async fn handle(&self, frame: Frame) -> Result<(), TransportError> {
        match frame {
            Frame::Request {
                protocol_version,
                request_id,
                type_,
                payload,
            } => {
                if protocol_version.0 != SUPPORTED_PROTOCOL_VERSION {
                    let received = protocol_version.0;
                    return self.send(error_response(
                        request_id,
                        "unsupported_protocol_version",
                        format!("unsupported protocol version {received:?}"),
                        Some(json!({ "supported": SUPPORTED_PROTOCOL_VERSION, "received": received })),
                    ));
                }
                self.route(&type_, request_id, payload).await
            }
            Frame::Response { request_id, .. } => self.send(error_response(
                request_id,
                "unexpected_kind",
                "host received a response frame; it only accepts requests",
                None,
            )),
            Frame::Notification {
                notification_id, ..
            } => self.send(error_response(
                notification_id,
                "unexpected_kind",
                "host received a notification frame; it only accepts requests",
                None,
            )),
        }
    }

    /// Route a version-checked request by its `type`.
    async fn route(
        &self,
        type_: &str,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        match type_ {
            "ping" => self.send(ok_response(
                request_id,
                json!({ "pong": true, "echo": payload.get("nonce").cloned().unwrap_or(Value::Null) }),
            )),
            "hello" => self.handle_hello(request_id),
            "classify_message" | "read_message" => self.handle_classify(request_id, payload).await,
            "new_mail" => self.handle_new_mail(payload).await,
            "draft_reply" => self.handle_draft(request_id, payload).await,
            "record_user_action" => self.handle_record(request_id, payload).await,
            "enroll_pipeline_item" => self.handle_enroll(request_id, payload).await,
            "update_pipeline_stage" => self.handle_update_stage(request_id, payload).await,
            "cancel_sequence" => self.handle_cancel(request_id, payload).await,
            "reschedule_followup" => self.handle_reschedule(request_id, payload, false).await,
            "snooze" => self.handle_reschedule(request_id, payload, true).await,
            "review_followup" => self.handle_review_followup(request_id, payload).await,
            "explain_decision" => self.handle_explain(request_id, payload).await,
            "list_recent_activity" => self.handle_list_recent_activity(request_id, payload).await,
            "list_pending_reviews" => self.handle_list_reviews(request_id).await,
            "review_rule_proposal" => self.handle_review_proposal(request_id, payload).await,
            "get_settings" => self.handle_get_settings(request_id),
            other => self.send(error_response(
                request_id,
                "unknown_request_type",
                format!("unknown request type {other:?}"),
                Some(json!({ "type": other })),
            )),
        }
    }

    /// Classify a selected message and return the guarded plan as suggestions (no apply).
    async fn handle_classify(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let parsed: ClassifyMessagePayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        let tb_id = parsed.thunderbird_message_id.clone();
        let message = parsed.into_message_data();
        match self
            .planning
            .handle_message(message, TriggerKind::NewMail)
            .await
        {
            Ok(outcome) => self.send(ok_response(
                request_id,
                classify_response_payload(&outcome, &tb_id),
            )),
            Err(e) => self.send(error_response(
                request_id,
                "classification_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Classify a background arrival, apply the allowed low-risk actions, and push the result.
    async fn handle_new_mail(&self, payload: Value) -> Result<(), TransportError> {
        let parsed: ClassifyMessagePayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            // A malformed background event has no request to correlate to; record it and drop.
            Err(e) => {
                let _ = self
                    .audit
                    .append(
                        AuditEntry::new("new_mail_rejected", Actor::Extension)
                            .with_payload(json!({ "error": e.to_string() })),
                    )
                    .await;
                return Ok(());
            }
        };
        let tb_id = parsed.thunderbird_message_id.clone();
        let message = parsed.into_message_data();
        // Capture the header metadata before the message is consumed, so the dashboard Review
        // card can show the real subject/sender (header metadata, always within `metadata`
        // retention — never body content).
        let subject = message.headers.subject.clone();
        let from = message.headers.from.clone();
        let outcome = match self.planning.handle_new_mail(message).await {
            Ok(o) => o,
            Err(e) => {
                let _ = self
                    .audit
                    .append(
                        AuditEntry::new("classification_failed", Actor::System).with_payload(
                            json!({ "thunderbird_message_id": tb_id, "error": e.to_string() }),
                        ),
                    )
                    .await;
                return Ok(());
            }
        };
        let applied = self.apply_allowed(&outcome.guarded_plan).await;
        let payload = classification_ready_payload(&outcome, &tb_id, &applied, &subject, &from);
        self.send(Frame::Notification {
            protocol_version: ProtocolVersion::default(),
            notification_id: format!("ntf_classify_{tb_id}"),
            type_: "classification_ready".to_owned(),
            payload,
        })
    }

    /// Apply every policy-allowed action through the mail client, auditing each, and return
    /// the ones that were actually applied (so the notification can list them).
    async fn apply_allowed(&self, plan: &GuardedActionPlan) -> Vec<PlannedAction> {
        let mut applied = Vec::new();
        for action in &plan.allowed_actions {
            let result = match action {
                PlannedAction::CreateDraft { draft } => self
                    .mail_client
                    .create_draft(draft.clone())
                    .await
                    .map(|_| ()),
                other => match planned_to_mail_action(other) {
                    Some(mail_action) => self.mail_client.apply(mail_action).await,
                    // RequireReview is a surfaced flag, never auto-applied.
                    None => continue,
                },
            };
            match result {
                Ok(()) => {
                    let entry = AuditEntry::new(event_type::ACTION_APPLIED, Actor::System)
                        .with_payload(serde_json::to_value(action).unwrap_or(Value::Null));
                    let _ = self.audit.append(entry).await;
                    applied.push(action.clone());
                }
                Err(e) => {
                    let entry = AuditEntry::new("action_apply_failed", Actor::System)
                        .with_payload(json!({ "action": action, "error": e.to_string() }));
                    let _ = self.audit.append(entry).await;
                }
            }
        }
        applied
    }

    /// Generate an advisory, review-required reply draft.
    async fn handle_draft(&self, request_id: String, payload: Value) -> Result<(), TransportError> {
        let parsed: DraftReplyPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        match self.draft.draft_reply(parsed.into_request()).await {
            Ok(draft) => self.send(ok_response(
                request_id,
                json!({
                    "draft_id": draft.draft_id,
                    "subject": draft.subject,
                    "body": draft.body,
                    "safety_notes": draft.safety_notes,
                    "requires_human_review": draft.requires_human_review,
                }),
            )),
            Err(e) => self.send(error_response(
                request_id,
                "draft_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Enroll a tracked quote/proposal: create a `pipeline_item` and arm a workflow on it.
    async fn handle_enroll(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(followups) = &self.followups else {
            return self.send(followups_not_configured(request_id));
        };
        let parsed: EnrollPipelineItemPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        let item_id = match followups
            .pipeline_items
            .insert(parsed.into_new_item())
            .await
        {
            Ok(id) => id,
            Err(e) => {
                return self.send(error_response(
                    request_id,
                    "enroll_failed",
                    e.to_string(),
                    None,
                ))
            }
        };
        match followups
            .workflow_engine
            .arm(item_id.clone(), parsed.workflow_def_id())
            .await
        {
            Ok(instance_id) => {
                let _ = self
                    .audit
                    .append(
                        AuditEntry::new("pipeline_item_enrolled", Actor::User).with_payload(
                            json!({
                                "pipeline_item_id": item_id.as_str(),
                                "workflow_instance_id": instance_id.as_str(),
                                "workflow_id": parsed.workflow_id,
                            }),
                        ),
                    )
                    .await;
                self.send(ok_response(
                    request_id,
                    json!({
                        "pipeline_item_id": item_id,
                        "workflow_instance_id": instance_id,
                    }),
                ))
            }
            Err(e) => self.send(error_response(
                request_id,
                "enroll_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Mark a deal won/lost (exits the sequence and closes the stage).
    async fn handle_update_stage(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(followups) = &self.followups else {
            return self.send(followups_not_configured(request_id));
        };
        let parsed: UpdatePipelineStagePayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        let Some(event) = parsed.exit_event() else {
            return self.send(error_response(
                request_id,
                "invalid_stage",
                format!(
                    "update_pipeline_stage expects won/lost, got {:?}",
                    parsed.stage
                ),
                None,
            ));
        };
        self.exit(request_id, followups, parsed.item_id(), event)
            .await
    }

    /// Cancel a sequence on a pipeline item.
    async fn handle_cancel(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(followups) = &self.followups else {
            return self.send(followups_not_configured(request_id));
        };
        let parsed: CancelSequencePayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        self.exit(request_id, followups, parsed.item_id(), ExitEvent::Cancel)
            .await
    }

    /// Drive an exit event and answer with the instances it exited.
    async fn exit(
        &self,
        request_id: String,
        followups: &FollowUpSuite,
        item: mailmate_common::ids::PipelineItemId,
        event: ExitEvent,
    ) -> Result<(), TransportError> {
        let item_id = item.clone();
        match followups.exit_detector.on_exit_event(item, event).await {
            Ok(exited) => {
                self.audit_exit(&item_id, event, &exited).await;
                self.send(ok_response(
                    request_id,
                    json!({ "exited": exited, "event": event.as_str() }),
                ))
            }
            Err(e) => self.send(error_response(
                request_id,
                "exit_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Audit a workflow exit (the mutable-state instance's transition is reconstructable from
    /// the audit timeline — see the data-model's mutable-state exception). Actor by origin: a
    /// reply is the system observing inbound mail; won/lost/cancel are user decisions.
    async fn audit_exit(
        &self,
        item: &mailmate_common::ids::PipelineItemId,
        event: ExitEvent,
        exited: &[mailmate_common::ids::WorkflowInstanceId],
    ) {
        let actor = match event {
            ExitEvent::ReplyReceived => Actor::System,
            ExitEvent::Won | ExitEvent::Lost | ExitEvent::Cancel => Actor::User,
        };
        let _ = self
            .audit
            .append(
                AuditEntry::new("workflow_exited", actor).with_payload(json!({
                    "pipeline_item_id": item.as_str(),
                    "event": event.as_str(),
                    "exited_instances": exited.iter().map(|i| i.as_str()).collect::<Vec<_>>(),
                })),
            )
            .await;
    }

    /// Push a follow-up's next step out (`reschedule_followup` / `snooze`).
    async fn handle_reschedule(
        &self,
        request_id: String,
        payload: Value,
        snooze: bool,
    ) -> Result<(), TransportError> {
        let Some(followups) = &self.followups else {
            return self.send(followups_not_configured(request_id));
        };
        let parsed: RescheduleFollowupPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        match followups
            .workflow_engine
            .reschedule(parsed.instance_id(), parsed.next_due_at, snooze)
            .await
        {
            Ok(()) => self.send(ok_response(
                request_id,
                json!({ "rescheduled": true, "snoozed": snooze }),
            )),
            Err(e) => self.send(error_response(
                request_id,
                "reschedule_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Resolve a surfaced follow-up draft (`review_followup`): advance the cursor. No
    /// resolution sends — `send` means the human already dispatched the draft.
    async fn handle_review_followup(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(followups) = &self.followups else {
            return self.send(followups_not_configured(request_id));
        };
        let parsed: ReviewFollowupPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        let Some(resolution) = parsed.resolution() else {
            return self.send(error_response(
                request_id,
                "invalid_resolution",
                format!(
                    "review_followup expects send/edit/skip, got {:?}",
                    parsed.resolution
                ),
                None,
            ));
        };
        match followups
            .workflow_engine
            .resolve_review(parsed.instance_id(), resolution, self.clock.now())
            .await
        {
            Ok(()) => self.send(ok_response(
                request_id,
                json!({ "resolved": true, "resolution": resolution.as_str() }),
            )),
            Err(e) => self.send(error_response(
                request_id,
                "review_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Explain a decision: return the audit timeline for one message (the classification,
    /// the applied/blocked actions, the corrections — the data any review/explanation UI
    /// renders). Uses the always-present audit store, so it needs no admin wiring.
    async fn handle_explain(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let parsed: ExplainDecisionPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        let Some(message_id) = parsed.message_id() else {
            return self.send(error_response(
                request_id,
                "invalid_payload",
                "explain_decision requires message_id or thunderbird_message_id",
                None,
            ));
        };
        let query = AuditQuery {
            message_id: Some(message_id.clone()),
            limit: parsed.limit,
            ..AuditQuery::default()
        };
        match self.audit.query(query).await {
            Ok(entries) => {
                let timeline: Vec<Value> = entries
                    .iter()
                    .map(|entry| {
                        json!({
                            "id": entry.id,
                            "event_type": entry.event_type,
                            "actor": entry.actor.as_str(),
                            "created_at": entry.created_at,
                            "payload": entry.payload,
                        })
                    })
                    .collect();
                self.send(ok_response(
                    request_id,
                    json!({ "message_id": message_id, "timeline": timeline }),
                ))
            }
            Err(e) => self.send(error_response(
                request_id,
                "explain_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Power the dashboard Activity tab's cross-message **global** stream. `explain_decision` is
    /// per-message (it requires and keys on one `message_id`); this drops that constraint and
    /// reads the newest audit entries, optionally narrowed to one of the six event-type
    /// *families* the UI's filter chips expose. Family matching happens here because the audit
    /// store keys on a single exact `event_type` (one family spans several event types), so we
    /// over-read when a filter is present and cap to `limit` after grouping. Read-only, served
    /// from the always-present audit store — no admin wiring required.
    async fn handle_list_recent_activity(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        // A missing/empty payload is a valid "everything, default limit" request.
        let parsed: ListRecentActivityPayload = serde_json::from_value(payload).unwrap_or_default();
        let limit = parsed.limit.unwrap_or(50).min(500);
        let family = parsed.event_type_filter.as_deref().filter(|f| *f != "all");
        // The store filters by exact event_type only, so when narrowing to a multi-type family
        // we over-read and group here. Bound the over-read so a pathological filter can't scan
        // the whole log.
        let fetch = if family.is_some() {
            limit.saturating_mul(8).min(2000)
        } else {
            limit
        };
        let query = AuditQuery {
            limit: Some(fetch),
            ..AuditQuery::default()
        };
        match self.audit.query(query).await {
            Ok(entries) => {
                let events: Vec<Value> = entries
                    .iter()
                    .filter(|e| activity_family_matches(family, &e.event_type))
                    .take(limit)
                    .map(|e| {
                        json!({
                            "id": e.id,
                            "event_type": e.event_type,
                            "actor": e.actor.as_str(),
                            "message_id": e.message_id,
                            "rule_id": e.rule_id,
                            "proposal_id": e.proposal_id,
                            "created_at": e.created_at,
                            "payload": e.payload,
                        })
                    })
                    .collect();
                self.send(ok_response(request_id, json!({ "events": events })))
            }
            Err(e) => self.send(error_response(
                request_id,
                "list_activity_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// List the agent proposals awaiting human review — the review UI's work queue.
    async fn handle_list_reviews(&self, request_id: String) -> Result<(), TransportError> {
        let Some(admin) = &self.admin else {
            return self.send(admin_not_configured(request_id));
        };
        match admin
            .proposals
            .list_by_status(ProposalStatus::PendingReview)
            .await
        {
            Ok(proposals) => {
                let pending: Vec<Value> = proposals
                    .iter()
                    .map(|p| {
                        json!({
                            "id": p.id,
                            "title": p.title,
                            "proposal_type": p.proposal_type.as_str(),
                            "recommended_status": p.recommended_status.as_str(),
                            "risk_level": p.risk_level.as_str(),
                            "rationale": p.rationale,
                        })
                    })
                    .collect();
                self.send(ok_response(
                    request_id,
                    json!({ "pending_reviews": pending }),
                ))
            }
            Err(e) => self.send(error_response(
                request_id,
                "list_reviews_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Apply a human's accept/reject decision to a pending agent proposal — the Proposals-tab
    /// materialization gate. Acceptance is the **only** path that creates the recommended rule,
    /// and even then it enters its recommended status (shadow / pending-review), never directly
    /// `active`: both `accept_*` decisions materialize to that recommended status (forcing a
    /// rule straight to `active` is a separate promotion the review port does not perform), and
    /// a rejection records the curator's negative signal. Served through the always-present
    /// proposal-review port, so it needs no admin wiring.
    async fn handle_review_proposal(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let parsed: ReviewRuleProposalPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        let proposal_id = ProposalId::from(parsed.proposal_id.as_str());
        let decision = match parsed.decision.as_str() {
            "accept_for_shadow_mode" | "accept_active" => ReviewDecision::accept(proposal_id),
            "reject" => ReviewDecision::reject(
                proposal_id,
                parsed.reason_code.unwrap_or_else(|| "rejected".to_owned()),
            ),
            other => {
                return self.send(error_response(
                    request_id,
                    "invalid_decision",
                    format!(
                        "decision must be accept_for_shadow_mode | accept_active | reject, got {other:?}"
                    ),
                    None,
                ));
            }
        };
        match self.proposal_review.review(decision).await {
            Ok(outcome) => self.send(ok_response(
                request_id,
                json!({
                    "reviewed": true,
                    "proposal_id": outcome.proposal_id,
                    "resulting_status": outcome.new_status.as_str(),
                    "rule_id": outcome.created_rule_id,
                }),
            )),
            Err(e) => self.send(error_response(
                request_id,
                "review_proposal_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Return the effective, secret-free settings snapshot.
    fn handle_get_settings(&self, request_id: String) -> Result<(), TransportError> {
        let Some(admin) = &self.admin else {
            return self.send(admin_not_configured(request_id));
        };
        let payload = serde_json::to_value(&admin.settings).unwrap_or_else(|_| json!({}));
        self.send(ok_response(request_id, payload))
    }

    /// Answer the `hello` handshake: a richer-than-`ping` round-trip carrying the host/protocol
    /// version, the capabilities this build has wired, and the secret-free drafting/retention
    /// posture. The extension uses it to render Connected / version-mismatch and to gate
    /// feature UI *before* sending any real request (and to guard the single-writer channel
    /// against a protocol it cannot speak). It exposes no secret — drafting availability is a
    /// bool, not a key.
    fn handle_hello(&self, request_id: String) -> Result<(), TransportError> {
        let mut capabilities = vec![
            "classify_message",
            "draft_reply",
            "record_user_action",
            "explain_decision",
            "list_recent_activity",
            "review_rule_proposal",
        ];
        if self.followups.is_some() {
            capabilities.push("followups");
        }
        if self.admin.is_some() {
            capabilities.push("list_pending_reviews");
            capabilities.push("get_settings");
        }
        // Drafting/retention come from the wired settings snapshot; an unwired admin surface
        // reports the host's safe local-first defaults (no provider, metadata retention).
        let (drafting_available, retention_level) = self.admin.as_ref().map_or_else(
            || (false, "metadata".to_owned()),
            |admin| {
                (
                    admin.settings.default_provider.is_some(),
                    admin.settings.retention_level.clone(),
                )
            },
        );
        self.send(ok_response(
            request_id,
            json!({
                "host_version": env!("CARGO_PKG_VERSION"),
                "protocol_version": SUPPORTED_PROTOCOL_VERSION,
                "capabilities": capabilities,
                "drafting_available": drafting_available,
                "retention_level": retention_level,
            }),
        ))
    }

    /// Run the catch-up-on-launch drain: surface a `followup_draft_ready` for each fired step
    /// (a review-required draft, never sent on arrival) and a `followup_needs_attention` for
    /// each stale instance. Host-initiated (not a request); `app.rs` calls it at startup and
    /// on a periodic tick (the production composition root is deferred to Phase 12).
    ///
    /// # Errors
    /// Propagates a [`TransportError`] from emitting a frame. A scheduler/storage failure is
    /// audited and ends the sweep without erroring the channel.
    pub async fn drain_followups(&self) -> Result<(), TransportError> {
        let Some(followups) = &self.followups else {
            return Ok(());
        };
        let report = match followups.scheduler.drain_due(self.clock.now()).await {
            Ok(r) => r,
            Err(e) => {
                let _ = self
                    .audit
                    .append(
                        AuditEntry::new("followup_drain_failed", Actor::System)
                            .with_payload(json!({ "error": e.to_string() })),
                    )
                    .await;
                return Ok(());
            }
        };
        for fired in &report.fired {
            self.send(Frame::Notification {
                protocol_version: ProtocolVersion::default(),
                notification_id: format!("ntf_followup_{}", fired.workflow_instance_id),
                type_: "followup_draft_ready".to_owned(),
                payload: followup_draft_ready_payload(fired),
            })?;
        }
        for item in &report.needs_attention {
            self.send(Frame::Notification {
                protocol_version: ProtocolVersion::default(),
                notification_id: format!("ntf_attention_{}", item.workflow_instance_id),
                type_: "followup_needs_attention".to_owned(),
                payload: followup_needs_attention_payload(item),
            })?;
        }
        Ok(())
    }

    /// Route a recorded user action to its single owner: a correction to its feedback table,
    /// or a provenance/execution fact to the audit log.
    async fn handle_record(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let parsed: RecordUserActionPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };

        let ack = match self.route_recorded(&parsed).await {
            Ok((sink, id)) => ok_response(
                request_id,
                json!({ "recorded": true, "sink": sink, "id": id }),
            ),
            Err(e) => error_response(request_id, "record_failed", e, None),
        };
        self.send(ack)
    }

    /// The routing decision for one recorded action. Returns `(sink, id)`.
    async fn route_recorded(
        &self,
        payload: &RecordUserActionPayload,
    ) -> Result<(&'static str, String), String> {
        match payload.event_type.as_str() {
            "junk_changed" => {
                let message_id = payload
                    .message_id()
                    .ok_or("junk_changed requires thunderbird_message_id")?;
                // The junk state is the discriminator between MarkSpam and MarkNotSpam, so it is
                // required: a missing field must error, not silently fabricate a spam label (which
                // would also feed the Tier-2 online update).
                let junk = payload.junk.ok_or("junk_changed requires junk")?;
                let correction = if junk {
                    UserCorrection::MarkSpam { message_id }
                } else {
                    UserCorrection::MarkNotSpam { message_id }
                };
                let id = self
                    .correction
                    .handle_correction(
                        correction,
                        FeatureVector::new(),
                        self.correction_ctx(payload),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(("classification_feedback", id.into_string()))
            }
            "message_moved" if payload.user_initiated => {
                let message_id = payload
                    .message_id()
                    .ok_or("message_moved requires thunderbird_message_id")?;
                let to_folder = payload
                    .to_folder_id
                    .clone()
                    .map(FolderId::from)
                    .ok_or("message_moved requires to_folder_id")?;
                let correction = UserCorrection::LearnFiling {
                    message_id,
                    to_folder,
                };
                let id = self
                    .correction
                    .handle_correction(
                        correction,
                        FeatureVector::new(),
                        self.correction_ctx(payload),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(("filing_feedback", id.into_string()))
            }
            // A one-click wrong-category correction → classification feedback.
            "classification_corrected" => self.route_label_correction(payload).await,
            // An undo of an auto-applied action → strong negative learning signal.
            "action_undone" => self.route_undo(payload).await,
            // A dismissed suggestion is the ignore-rate signal. The feedback tables key on a
            // *chosen* label/folder, which a dismiss does not supply; fabricating one would be
            // a false signal (cf. the junk-without-junk guard), so it is captured as audit
            // provenance — queryable for ignore/dismiss-rate — carrying action_kind + authored_by.
            "suggestion_dismissed" => self.audit_recorded(payload).await,
            // A reply on a tracked thread exits the follow-up sequence (when follow-ups are
            // wired); otherwise it falls through to the audit arm as plain provenance.
            "reply_received" if self.followups.is_some() => self.route_reply(payload).await,
            // Everything else is pure provenance / an execution result: audit only.
            _ => self.audit_recorded(payload).await,
        }
    }

    /// Record an event as plain audit provenance (the non-correction arm): an execution
    /// result, a pure observation, or a signal with no learned-task feedback table. Returns
    /// `("audit", id)`.
    async fn audit_recorded(
        &self,
        payload: &RecordUserActionPayload,
    ) -> Result<(&'static str, String), String> {
        // Attribute by origin: an execution result is the extension's; a move that reaches this
        // arm is host-initiated (a user move took the correction arm above), so it is the
        // system's; the rest are user-observed behavior.
        let event = payload.event_type.as_str();
        let actor = match event {
            "action_applied" | "action_failed" => Actor::Extension,
            "message_moved" => Actor::System,
            _ => Actor::User,
        };
        let mut entry = AuditEntry::new(event, actor).with_payload(provenance_payload(payload));
        if let Some(message_id) = payload.message_id() {
            entry = entry.with_message(message_id);
        }
        let id = self.audit.append(entry).await.map_err(|e| e.to_string())?;
        Ok(("audit", id.into_string()))
    }

    /// Route a one-click wrong-category correction into classification feedback. The prior
    /// label flows in as the AI label so the captured row's polarity records the override
    /// honestly (a diverging label is `Negative`, an agreeing one is reinforcement).
    async fn route_label_correction(
        &self,
        payload: &RecordUserActionPayload,
    ) -> Result<(&'static str, String), String> {
        let message_id = payload
            .message_id()
            .ok_or("classification_corrected requires thunderbird_message_id")?;
        let label = payload
            .corrected_label
            .clone()
            .ok_or("classification_corrected requires corrected_label")?;
        let mut context = self.correction_ctx(payload);
        context.ai_label = payload.prior_label.clone();
        let id = self
            .correction
            .handle_correction(
                UserCorrection::CorrectLabel { message_id, label },
                FeatureVector::new(),
                context,
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(("classification_feedback", id.into_string()))
    }

    /// Route an undo of an auto-applied action into learning as strong negative evidence
    /// against the rule that fired. A reverted *move* teaches the filing it was put back to
    /// (the rule's now-undone target is the diverged AI suggestion → negative for that
    /// folder); an undone *junk-mark* is a not-spam correction; a reverted tag/draft has no
    /// learned-task feedback table, so it is recorded as provenance.
    async fn route_undo(
        &self,
        payload: &RecordUserActionPayload,
    ) -> Result<(&'static str, String), String> {
        let message_id = payload
            .message_id()
            .ok_or("action_undone requires thunderbird_message_id")?;
        match payload.action_kind.as_deref() {
            Some("move") => {
                let to_folder =
                    payload.to_folder_id.clone().map(FolderId::from).ok_or(
                        "action_undone(move) requires to_folder_id (the reverted-to folder)",
                    )?;
                let mut context = self.correction_ctx(payload);
                // The rule's now-undone target (where the message was) is the AI-suggested
                // folder the human diverged from by reverting it.
                context.ai_suggested_folder = payload.from_folder_id.clone().map(FolderId::from);
                let id = self
                    .correction
                    .handle_correction(
                        UserCorrection::LearnFiling {
                            message_id,
                            to_folder,
                        },
                        FeatureVector::new(),
                        context,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(("filing_feedback", id.into_string()))
            }
            // A rule only ever auto-*marks* junk; undoing it is a not-spam correction.
            Some("mark_junk" | "junk") => {
                let id = self
                    .correction
                    .handle_correction(
                        UserCorrection::MarkNotSpam { message_id },
                        FeatureVector::new(),
                        self.correction_ctx(payload),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(("classification_feedback", id.into_string()))
            }
            _ => self.audit_recorded(payload).await,
        }
    }

    /// Exit every tracked sequence on the reply's thread (host-side thread identity). The
    /// guard in `route_recorded` ensures the follow-up suite is present before this is called.
    async fn route_reply(
        &self,
        payload: &RecordUserActionPayload,
    ) -> Result<(&'static str, String), String> {
        let followups = self
            .followups
            .as_ref()
            .ok_or("reply_received requires the follow-up suite")?;
        let thread = payload
            .thread_id
            .clone()
            .ok_or("reply_received requires thread_id")?;
        let thread_id = ThreadId::from(thread);
        let items = followups
            .pipeline_items
            .get_by_thread(&thread_id)
            .await
            .map_err(|e| e.to_string())?;
        let mut exited = Vec::new();
        for item in items {
            let item_id = item.id.clone();
            let ids = followups
                .exit_detector
                .on_exit_event(item.id, ExitEvent::ReplyReceived)
                .await
                .map_err(|e| e.to_string())?;
            self.audit_exit(&item_id, ExitEvent::ReplyReceived, &ids)
                .await;
            exited.extend(
                ids.into_iter()
                    .map(mailmate_common::ids::WorkflowInstanceId::into_string),
            );
        }
        Ok(("workflow_exit", exited.join(",")))
    }

    /// Build the correction context from the wire payload's optional hints.
    fn correction_ctx(&self, payload: &RecordUserActionPayload) -> CorrectionContext {
        CorrectionContext {
            sender_domain: payload.sender_domain.clone(),
            ai_suggested_folder: payload.ai_suggested_folder.clone().map(FolderId::from),
            ..CorrectionContext::default()
        }
    }

    /// Send one frame through the output transport.
    fn send(&self, frame: Frame) -> Result<(), TransportError> {
        self.out.send(frame)
    }
}

/// A schema-parse failure becomes a correlated `invalid_payload` error response.
fn invalid_payload(request_id: String, err: &serde_json::Error) -> Frame {
    error_response(
        request_id,
        "invalid_payload",
        format!("payload did not match the expected schema: {err}"),
        None,
    )
}

/// The error response for a follow-up request when the suite is not wired (the binary's
/// composition root that injects it is deferred to Phase 12).
fn followups_not_configured(request_id: String) -> Frame {
    error_response(
        request_id,
        "followups_not_configured",
        "the follow-up workflow engine is not wired into this host build",
        None,
    )
}

/// The error response for an admin request when the management surface is not wired.
fn admin_not_configured(request_id: String) -> Frame {
    error_response(
        request_id,
        "admin_not_configured",
        "the management surface (proposals/settings) is not wired into this host build",
        None,
    )
}

/// Does an audit entry's exact `event_type` belong to the requested Activity-tab *family*?
/// `None` (or the sentinel `"all"`, already stripped by the caller) matches everything. The
/// families mirror the dashboard's filter chips and group the host's concrete event types — a
/// failure variant lives with its family (a failed apply is still "applied"; a rejected
/// classification is still "classified") so the stream never silently hides a real event. An
/// unrecognised family is permissive (matches all) rather than blanking the view.
fn activity_family_matches(family: Option<&str>, event_type: &str) -> bool {
    let Some(family) = family else {
        return true;
    };
    match family {
        "classified" => matches!(
            event_type,
            "classified" | "classification_failed" | "new_mail_rejected"
        ),
        // Every spelling a failed apply can arrive under lives with the family: the host's own
        // `action_apply_failed` (over-the-wire send error) and the extension's `action_failed`
        // (the async Thunderbird-layer failure, the common case) — never hide a real failure.
        "applied" => matches!(
            event_type,
            "action_applied" | "action_apply_failed" | "action_failed"
        ),
        // A discarded model output is a refused action — it belongs with the policy-blocked family.
        "blocked" => matches!(
            event_type,
            "action_blocked_by_policy" | "provider_response_rejected"
        ),
        "corrected" => matches!(
            event_type,
            "suggestion_dismissed"
                | "classification_corrected"
                | "action_undone"
                | "junk_changed"
                | "message_moved"
        ),
        // Enrollment + exit (router-authored) AND the scheduler's per-step lifecycle events,
        // which share the same audit store — so a fired/coalesced/stale/failed step is never
        // dropped from the Follow-ups view.
        "follow_up" => matches!(
            event_type,
            "pipeline_item_enrolled"
                | "workflow_exited"
                | "followup_drain_failed"
                | "followup_step_fired"
                | "followup_coalesced"
                | "followup_needs_attention"
                | "followup_draft_failed"
                | "followup_version_missing"
        ),
        // Rule-proposal lifecycle, including the `workflow_status_changed` a review materializes.
        "proposal" => matches!(
            event_type,
            "rule_proposed"
                | "proposal_reviewed"
                | "rule_status_changed"
                | "rule_conflict_detected"
                | "workflow_status_changed"
        ),
        // An unknown filter must not blank the stream — show everything.
        _ => true,
    }
}

/// Project a safe [`PlannedAction`] onto the [`MailAction`] the client applies, or `None` for
/// actions that are not a direct mail mutation (`CreateDraft`, `RequireReview`).
fn planned_to_mail_action(action: &PlannedAction) -> Option<MailAction> {
    match action {
        PlannedAction::Tag { message_id, tag } => Some(MailAction::Tag {
            message_id: message_id.clone(),
            tag: tag.clone(),
        }),
        PlannedAction::Move {
            message_id,
            to_folder,
        } => Some(MailAction::Move {
            message_id: message_id.clone(),
            to_folder: to_folder.clone(),
        }),
        PlannedAction::MarkJunk { message_id, junk } => Some(MailAction::MarkJunk {
            message_id: message_id.clone(),
            junk: *junk,
        }),
        PlannedAction::CreateDraft { .. } | PlannedAction::RequireReview { .. } => None,
    }
}

/// The compact provenance payload an audited event records.
fn provenance_payload(p: &RecordUserActionPayload) -> Value {
    json!({
        "event_type": p.event_type,
        "thunderbird_message_id": p.thunderbird_message_id,
        "from_folder_id": p.from_folder_id,
        "to_folder_id": p.to_folder_id,
        "junk": p.junk,
        "read": p.read,
        "tag": p.tag,
        "user_initiated": p.user_initiated,
        "result": p.result,
        // Carried so the curator can compute ignore/undo-rate from the audit stream for
        // signals (dismissals, tag/draft undos) that have no learned-task feedback table.
        "action_kind": p.action_kind,
        "authored_by": p.authored_by,
        "rule_id": p.rule_id,
    })
}
