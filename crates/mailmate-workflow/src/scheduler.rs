//! [`DefaultFollowUpScheduler`]: the catch-up-on-launch drain. For each due instance it
//! applies the pure cadence guard ([`crate::cadence::evaluate_due`]), drives the existing
//! reply-drafter to produce a **review-required** draft (never a send), records the
//! cadence/timing signal in `followup_feedback`, audits the firing, advances the instance,
//! and returns a [`DrainReport`] the host turns into `followup_draft_ready` /
//! `followup_needs_attention` frames.
//!
//! A fired step's draft is review-required **by construction** — it is minted via
//! [`ReplyDraft::from_drafted`], which pins `requires_human_review = true`; there is no send
//! vocabulary anywhere in this path, so `never_auto_send_drafts` holds.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use mailmate_common::actor::Actor;
use mailmate_common::audit::AuditEntry;
use mailmate_common::error::WorkflowError;
use mailmate_common::feedback::{
    FeedbackPolarity, FollowUpFeedback, FollowUpFeedbackRow, FollowUpOutcome, PinnedVersions,
};
use mailmate_common::ids::{DecisionId, DraftId};
use mailmate_common::pipeline::PipelineItem;
use mailmate_common::reply::{ReplyDraft, ReplyDraftRequest};
use mailmate_common::time::Timestamp;
use mailmate_common::workflow::{
    DrainReport, FiredStep, FollowUpStep, NeedsAttentionItem, WorkflowDefinitionVersion,
    WorkflowInstance, WorkflowInstanceStatus,
};
use mailmate_ports::follow_up_scheduler::FollowUpScheduler;
use mailmate_ports::reply_drafter::ReplyDrafter;
use mailmate_ports::storage::audit::AuditRepository;
use mailmate_ports::storage::feedback::FeedbackRepository;
use mailmate_ports::storage::pipeline_items::PipelineItemRepository;
use mailmate_ports::storage::workflows::{WorkflowInstanceRepository, WorkflowRepository};

use crate::cadence::{evaluate_due, DrainDecision};

/// Audit event types the scheduler emits.
mod event {
    pub(super) const STEP_FIRED: &str = "followup_step_fired";
    pub(super) const COALESCED: &str = "followup_coalesced";
    pub(super) const NEEDS_ATTENTION: &str = "followup_needs_attention";
    pub(super) const DRAFT_FAILED: &str = "followup_draft_failed";
    pub(super) const VERSION_MISSING: &str = "followup_version_missing";
}

/// The default follow-up scheduler over the repository, drafter, and audit ports.
#[derive(Clone)]
pub struct DefaultFollowUpScheduler {
    workflows: Arc<dyn WorkflowRepository>,
    instances: Arc<dyn WorkflowInstanceRepository>,
    items: Arc<dyn PipelineItemRepository>,
    feedback: Arc<dyn FeedbackRepository<FollowUpFeedback>>,
    drafter: Arc<dyn ReplyDrafter>,
    audit: Arc<dyn AuditRepository>,
}

impl DefaultFollowUpScheduler {
    /// Assemble the scheduler from its ports.
    #[must_use]
    pub fn new(
        workflows: Arc<dyn WorkflowRepository>,
        instances: Arc<dyn WorkflowInstanceRepository>,
        items: Arc<dyn PipelineItemRepository>,
        feedback: Arc<dyn FeedbackRepository<FollowUpFeedback>>,
        drafter: Arc<dyn ReplyDrafter>,
        audit: Arc<dyn AuditRepository>,
    ) -> Self {
        Self {
            workflows,
            instances,
            items,
            feedback,
            drafter,
            audit,
        }
    }

    /// Best-effort audit: a logging failure must not abort the whole drain pass.
    async fn audit_best_effort(&self, entry: AuditEntry) {
        let _ = self.audit.append(entry).await;
    }

    /// Handle one due instance, appending to `report`.
    async fn drain_instance(
        &self,
        inst: &WorkflowInstance,
        now: Timestamp,
        report: &mut DrainReport,
    ) -> Result<(), WorkflowError> {
        let version = match self
            .workflows
            .get_version(&inst.pinned_def_version_id)
            .await?
        {
            Some(v) => v,
            None => {
                self.audit_best_effort(
                    AuditEntry::new(event::VERSION_MISSING, Actor::System).with_payload(json!({
                        "workflow_instance_id": inst.id.as_str(),
                        "pinned_def_version_id": inst.pinned_def_version_id.as_str(),
                    })),
                )
                .await;
                return Ok(());
            }
        };
        let item = match self.items.get(&inst.pipeline_item_id).await? {
            Some(i) => i,
            None => {
                self.audit_best_effort(
                    AuditEntry::new(event::VERSION_MISSING, Actor::System).with_payload(json!({
                        "workflow_instance_id": inst.id.as_str(),
                        "pipeline_item_id": inst.pipeline_item_id.as_str(),
                        "reason": "pipeline item missing",
                    })),
                )
                .await;
                return Ok(());
            }
        };

        match evaluate_due(
            &version.content,
            inst.anchor_at,
            inst.current_step_index,
            now,
        ) {
            DrainDecision::Fire {
                step_index,
                coalesced_from,
            } => {
                self.fire_step(
                    inst,
                    &item,
                    &version,
                    step_index,
                    coalesced_from,
                    now,
                    report,
                )
                .await
            }
            DrainDecision::NeedsAttention {
                skipped_step_indexes,
            } => {
                self.mark_needs_attention(inst, &item, skipped_step_indexes, now, report)
                    .await
            }
            DrainDecision::Nothing => Ok(()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn fire_step(
        &self,
        inst: &WorkflowInstance,
        item: &PipelineItem,
        version: &WorkflowDefinitionVersion,
        step_index: i64,
        coalesced_from: Vec<i64>,
        now: Timestamp,
        report: &mut DrainReport,
    ) -> Result<(), WorkflowError> {
        let Some(step) = version.content.step(step_index) else {
            return Ok(());
        };
        let request = build_request(item, step);
        let drafted = match self.drafter.draft(request).await {
            Ok(d) => d,
            Err(e) => {
                // Leave the instance armed so the next drain retries; never auto-skip a step.
                self.audit_best_effort(
                    AuditEntry::new(event::DRAFT_FAILED, Actor::System).with_payload(json!({
                        "workflow_instance_id": inst.id.as_str(),
                        "step_index": step_index,
                        "error": e.to_string(),
                    })),
                )
                .await;
                return Ok(());
            }
        };
        // Review-required by construction: from_drafted pins requires_human_review = true.
        let draft = ReplyDraft::from_drafted(DraftId::fresh(), drafted);
        let decision_id = DecisionId::fresh();

        self.feedback
            .append(FollowUpFeedbackRow {
                id: FollowUpFeedback::fresh_id(),
                workflow_instance_id: inst.id.clone(),
                pipeline_item_id: item.id.clone(),
                step_index,
                draft_id: Some(draft.draft_id.clone()),
                pinned_versions: PinnedVersions::default(),
                ai_scheduled_offset_days: step.offset_days,
                actual_offset_days: None,
                reply_received_before_step: false,
                reply_latency_days: None,
                outcome: FollowUpOutcome::SurfacedForReview,
                coalesced_from: coalesced_from.clone(),
                human_reason_code: None,
                human_reason_text: None,
                polarity: FeedbackPolarity::Positive,
                created_at: now,
            })
            .await?;

        self.audit_best_effort(
            AuditEntry::new(event::STEP_FIRED, Actor::System).with_payload(json!({
                "decision_id": decision_id.as_str(),
                "workflow_instance_id": inst.id.as_str(),
                "pipeline_item_id": item.id.as_str(),
                "step_index": step_index,
                "coalesced_from": coalesced_from,
                "draft_id": draft.draft_id.as_str(),
            })),
        )
        .await;
        if !coalesced_from.is_empty() {
            self.audit_best_effort(
                AuditEntry::new(event::COALESCED, Actor::System).with_payload(json!({
                    "workflow_instance_id": inst.id.as_str(),
                    "fired_step_index": step_index,
                    "coalesced_from": coalesced_from,
                })),
            )
            .await;
        }

        // The frequency cap: awaiting_review clears next_due_at → no new fire until resolved.
        self.instances
            .update_state(
                &inst.id,
                WorkflowInstanceStatus::AwaitingReview,
                step_index,
                None,
            )
            .await?;

        report.fired.push(FiredStep {
            workflow_instance_id: inst.id.clone(),
            pipeline_item_id: item.id.clone(),
            thread_id: inst.thread_id.clone(),
            step_index,
            coalesced_from,
            draft,
        });
        Ok(())
    }

    async fn mark_needs_attention(
        &self,
        inst: &WorkflowInstance,
        item: &PipelineItem,
        skipped: Vec<i64>,
        now: Timestamp,
        report: &mut DrainReport,
    ) -> Result<(), WorkflowError> {
        let latest_skipped = skipped
            .iter()
            .copied()
            .max()
            .unwrap_or(inst.current_step_index);
        self.feedback
            .append(FollowUpFeedbackRow {
                id: FollowUpFeedback::fresh_id(),
                workflow_instance_id: inst.id.clone(),
                pipeline_item_id: item.id.clone(),
                step_index: latest_skipped,
                draft_id: None,
                pinned_versions: PinnedVersions::default(),
                ai_scheduled_offset_days: 0,
                actual_offset_days: None,
                reply_received_before_step: false,
                reply_latency_days: None,
                outcome: FollowUpOutcome::ExpiredNeedsAttention,
                coalesced_from: skipped.clone(),
                human_reason_code: None,
                human_reason_text: None,
                polarity: FeedbackPolarity::Negative,
                created_at: now,
            })
            .await?;
        self.audit_best_effort(
            AuditEntry::new(event::NEEDS_ATTENTION, Actor::System).with_payload(json!({
                "workflow_instance_id": inst.id.as_str(),
                "pipeline_item_id": item.id.as_str(),
                "skipped_step_indexes": skipped,
            })),
        )
        .await;
        self.instances
            .update_state(
                &inst.id,
                WorkflowInstanceStatus::NeedsAttention,
                inst.current_step_index,
                None,
            )
            .await?;
        report.needs_attention.push(NeedsAttentionItem {
            workflow_instance_id: inst.id.clone(),
            pipeline_item_id: item.id.clone(),
            reason: "stale_past_horizon".to_owned(),
            skipped_step_indexes: skipped,
        });
        Ok(())
    }
}

/// Build the reply-draft request for a fired follow-up step. The step contributes the
/// drafting intent + the forbidden commitments; the anchor message + thread come from the
/// pipeline item — exactly the inputs the existing drafter expects.
fn build_request(item: &PipelineItem, step: &FollowUpStep) -> ReplyDraftRequest {
    ReplyDraftRequest {
        thread_id: Some(item.thread_id.clone()),
        in_reply_to: item.anchor_message_id.clone(),
        subject: format!("Re: {}", item.title),
        counterparty: item.counterparty_email.clone(),
        excerpt: item.title.clone(),
        user_instruction: Some(
            step.prompt_template_ref
                .clone()
                .unwrap_or_else(|| step.draft_intent.clone()),
        ),
        forbidden_commitments: step.forbidden_commitments.clone(),
    }
}

#[async_trait]
impl FollowUpScheduler for DefaultFollowUpScheduler {
    async fn drain_due(
        &self,
        now: Timestamp,
        batch_cap: usize,
    ) -> Result<DrainReport, WorkflowError> {
        // The cap is pushed into the query (LIMIT), so a long-offline backlog loads and fires in
        // one bounded batch — the soonest-due first — and the remainder drains on the next tick.
        let due = self.instances.list_due(now, batch_cap).await?;
        let mut report = DrainReport::default();
        for inst in &due {
            self.drain_instance(inst, now, &mut report).await?;
        }
        Ok(report)
    }

    async fn recover(&self) -> Result<(), WorkflowError> {
        // Catch-up-on-launch is idempotent: a step only advances an instance after its draft
        // is produced and persisted, so there is no half-fired state to repair. Recovery is
        // the startup drain itself (driven by the host's app loop in a later phase).
        Ok(())
    }
}
