//! The host router: protocol frames in, core use-cases driven, frames out.
//!
//! [`HostRouter`] is the application seam Phase 10 adds — the translator between the
//! Thunderbird wire protocol and the core's use-cases. It owns the [`PlanningService`],
//! [`CorrectionService`], and [`DraftService`] (assembled from the injected [`Ports`]), the
//! [`MailClient`] it applies safe actions through, and the [`AuditRepository`] it records
//! provenance into. Every output frame leaves through one injected [`Transport`], so the
//! single-writer guard keeps responses, notifications, and mail commands from interleaving.
//!
//! The router names only ports and use-cases — no concrete engine, provider, or backend — so it
//! is *intended* to be wired to the real adapters in production (the composition root that injects
//! them is deferred to Phase 12 hardening) and is exercised here against the in-memory fakes, with
//! no change to the code here. The shipped binary still runs the Phase-1 ping loop until that
//! wiring lands; see [`crate::dispatch`] and `main.rs`.
//!
//! ## What each request does
//! - `ping` — liveness echo (the Phase-1 contract, now served here too).
//! - `classify_message` / `read_message` — classify a *selected* message and return the
//!   guarded plan as suggestions; nothing is applied (the user is in the loop).
//! - `new_mail` — classify a *background* arrival, **apply** the policy-allowed low-risk
//!   actions through the [`MailClient`], and push a `classification_ready` notification that
//!   surfaces the review-required ones. This is "apply safe actions returned by the host".
//! - `draft_reply` — generate an advisory, review-required draft.
//! - `record_user_action` — route a correction to its per-task feedback table, or a pure
//!   provenance/execution-result fact to the audit log — never to both.

use std::io::Read;
use std::sync::Arc;

use futures::executor::block_on;
use serde_json::{json, Value};

use mailmate_common::action::{GuardedActionPlan, PlannedAction};
use mailmate_common::actor::Actor;
use mailmate_common::audit::{event_type, AuditEntry};
use mailmate_common::correction::UserCorrection;
use mailmate_common::error::TransportError;
use mailmate_common::features::FeatureVector;
use mailmate_common::ids::{FolderId, ThreadId};
use mailmate_common::mail::MailAction;
use mailmate_common::policy::TriggerKind;
use mailmate_common::protocol::{Frame, ProtocolVersion};
use mailmate_common::workflow::ExitEvent;
use mailmate_core::{CorrectionContext, CorrectionService, DraftService, PlanningService, Ports};
use mailmate_ports::clock::Clock;
use mailmate_ports::exit_detector::ExitDetector;
use mailmate_ports::follow_up_scheduler::FollowUpScheduler;
use mailmate_ports::mail_client::MailClient;
use mailmate_ports::storage::pipeline_items::PipelineItemRepository;
use mailmate_ports::storage::AuditRepository;
use mailmate_ports::transport::Transport;
use mailmate_ports::workflow_engine::WorkflowEngine;

use crate::convert::{
    classification_ready_payload, classify_response_payload, followup_draft_ready_payload,
    followup_needs_attention_payload,
};
use crate::dispatch::{error_response, ok_response, SUPPORTED_PROTOCOL_VERSION};
use crate::native_stdio::read_frame;
use crate::protocol_dto::{
    CancelSequencePayload, ClassifyMessagePayload, DraftReplyPayload, EnrollPipelineItemPayload,
    RecordUserActionPayload, RescheduleFollowupPayload, ReviewFollowupPayload,
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

/// Routes protocol frames into the core and emits the resulting frames.
#[derive(Clone)]
pub struct HostRouter {
    planning: PlanningService,
    correction: CorrectionService,
    draft: DraftService,
    mail_client: Arc<dyn MailClient>,
    audit: Arc<dyn AuditRepository>,
    clock: Arc<dyn Clock>,
    out: Arc<dyn Transport>,
    followups: Option<FollowUpSuite>,
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
            out,
            followups: None,
        }
    }

    /// Wire the follow-up engine suite, enabling the sales-pipeline control requests and the
    /// [`drain_followups`](Self::drain_followups) sweep.
    #[must_use]
    pub fn with_followups(mut self, followups: FollowUpSuite) -> Self {
        self.followups = Some(followups);
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
        let payload = classification_ready_payload(&outcome, &tb_id, &applied);
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
            // A reply on a tracked thread exits the follow-up sequence (when follow-ups are
            // wired); otherwise it falls through to the audit arm as plain provenance.
            "reply_received" if self.followups.is_some() => self.route_reply(payload).await,
            // Everything else is pure provenance / an execution result: audit only.
            other => {
                // Attribute by origin: an execution result is the extension's; a move that
                // reaches this arm is host-initiated (a user move took the correction arm above),
                // so it is the system's; the rest are user-observed behavior.
                let actor = match other {
                    "action_applied" | "action_failed" => Actor::Extension,
                    "message_moved" => Actor::System,
                    _ => Actor::User,
                };
                let mut entry =
                    AuditEntry::new(other, actor).with_payload(provenance_payload(payload));
                if let Some(message_id) = payload.message_id() {
                    entry = entry.with_message(message_id);
                }
                let id = self.audit.append(entry).await.map_err(|e| e.to_string())?;
                Ok(("audit", id.into_string()))
            }
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
    })
}
