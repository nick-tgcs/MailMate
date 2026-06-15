//! [`DefaultWorkflowEngine`]: arms a pipeline item on a workflow, detects containment
//! conflicts, and applies the user's control actions (reschedule/snooze, resolve a surfaced
//! review). It composes the workflow/instance/item repository ports and a clock; it drives
//! the cadence FSM (in [`crate::cadence`]) but never plans, guards, or sends.

use std::sync::Arc;

use async_trait::async_trait;

use mailmate_common::error::WorkflowError;
use mailmate_common::ids::{PipelineItemId, WorkflowDefId, WorkflowInstanceId};
use mailmate_common::pipeline::PipelineItem;
use mailmate_common::time::Timestamp;
use mailmate_common::workflow::{
    NewWorkflowInstance, ReviewResolution, WorkflowAnchor, WorkflowConflict,
    WorkflowDefinitionVersion, WorkflowInstanceStatus,
};
use mailmate_ports::clock::Clock;
use mailmate_ports::storage::pipeline_items::PipelineItemRepository;
use mailmate_ports::storage::workflows::{WorkflowInstanceRepository, WorkflowRepository};
use mailmate_ports::workflow_engine::WorkflowEngine;

use crate::cadence::{advance_after_review, next_due_after};
use crate::conflict::detect_conflicts;

/// The default workflow engine over the repository + clock ports.
#[derive(Clone)]
pub struct DefaultWorkflowEngine {
    workflows: Arc<dyn WorkflowRepository>,
    instances: Arc<dyn WorkflowInstanceRepository>,
    items: Arc<dyn PipelineItemRepository>,
    clock: Arc<dyn Clock>,
}

impl DefaultWorkflowEngine {
    /// Assemble the engine from its ports.
    #[must_use]
    pub fn new(
        workflows: Arc<dyn WorkflowRepository>,
        instances: Arc<dyn WorkflowInstanceRepository>,
        items: Arc<dyn PipelineItemRepository>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            workflows,
            instances,
            items,
            clock,
        }
    }

    async fn load_version(
        &self,
        id: &mailmate_common::ids::WorkflowDefVersionId,
    ) -> Result<WorkflowDefinitionVersion, WorkflowError> {
        self.workflows
            .get_version(id)
            .await?
            .ok_or_else(|| WorkflowError::NotFound(format!("workflow version {id}")))
    }
}

/// Resolve the anchor timestamp the offsets count from, for this item + anchor kind.
fn resolve_anchor(item: &PipelineItem, anchor: WorkflowAnchor) -> Timestamp {
    match anchor {
        // The most recent outbound activity (the quote send / tag at enrollment).
        WorkflowAnchor::QuoteSentAt | WorkflowAnchor::LastOutboundAt => item.last_activity_at,
        WorkflowAnchor::ItemCreatedAt => item.created_at,
    }
}

#[async_trait]
impl WorkflowEngine for DefaultWorkflowEngine {
    async fn arm(
        &self,
        item: PipelineItemId,
        workflow: WorkflowDefId,
    ) -> Result<WorkflowInstanceId, WorkflowError> {
        let pipeline_item = self
            .items
            .get(&item)
            .await?
            .ok_or_else(|| WorkflowError::NotFound(format!("pipeline item {item}")))?;
        let definition = self
            .workflows
            .get_definition(&workflow)
            .await?
            .ok_or_else(|| WorkflowError::NotFound(format!("workflow definition {workflow}")))?;
        let version = self.load_version(&definition.current_version_id).await?;

        let anchor_at = resolve_anchor(&pipeline_item, version.content.anchor);
        let first_due = next_due_after(&version.content, anchor_at, 0);
        // An empty cadence completes immediately rather than arming with no trigger (which
        // would violate the `next_due_at` non-NULL-iff-active invariant).
        let (status, next_due_at) = match first_due {
            Some(due) => (WorkflowInstanceStatus::Active, Some(due)),
            None => (WorkflowInstanceStatus::Completed, None),
        };

        let id = self
            .instances
            .arm(NewWorkflowInstance {
                pipeline_item_id: item,
                workflow_id: workflow,
                pinned_def_version_id: definition.current_version_id,
                thread_id: pipeline_item.thread_id,
                anchor_at,
                status,
                current_step_index: 0,
                next_due_at,
            })
            .await?;
        Ok(id)
    }

    async fn detect_conflicts(
        &self,
        item: &PipelineItemId,
        workflow: &WorkflowDefId,
    ) -> Result<Vec<WorkflowConflict>, WorkflowError> {
        let existing = self.instances.list_by_pipeline_item(item).await?;
        Ok(detect_conflicts(
            item,
            workflow,
            &existing,
            self.clock.now(),
        ))
    }

    async fn reschedule(
        &self,
        instance: WorkflowInstanceId,
        next_due_at: Timestamp,
        snooze: bool,
    ) -> Result<(), WorkflowError> {
        let inst = self
            .instances
            .get(&instance)
            .await?
            .ok_or_else(|| WorkflowError::NotFound(format!("workflow instance {instance}")))?;
        // Reschedule/snooze only pushes out an *armed* step. During `awaiting_review` the
        // cursor points at the already-fired step, so re-arming there would re-fire it on the
        // next drain (a duplicate review-required draft + a duplicate cadence-feedback row,
        // breaking the one-pending-draft cap). The FSM table has no `awaiting_review →
        // active` reschedule edge — reject it, leaving `review_followup` as the resolution.
        if !matches!(
            inst.status,
            WorkflowInstanceStatus::Active | WorkflowInstanceStatus::Snoozed
        ) {
            return Err(WorkflowError::InvalidState(format!(
                "instance {instance} is {}, cannot reschedule (only active/snoozed)",
                inst.status.as_str()
            )));
        }
        let status = if snooze {
            WorkflowInstanceStatus::Snoozed
        } else {
            WorkflowInstanceStatus::Active
        };
        self.instances
            .update_state(
                &instance,
                status,
                inst.current_step_index,
                Some(next_due_at),
            )
            .await?;
        Ok(())
    }

    async fn resolve_review(
        &self,
        instance: WorkflowInstanceId,
        _resolution: ReviewResolution,
        _now: Timestamp,
    ) -> Result<(), WorkflowError> {
        let inst = self
            .instances
            .get(&instance)
            .await?
            .ok_or_else(|| WorkflowError::NotFound(format!("workflow instance {instance}")))?;
        if inst.status != WorkflowInstanceStatus::AwaitingReview {
            return Err(WorkflowError::InvalidState(format!(
                "instance {instance} is {}, not awaiting_review",
                inst.status.as_str()
            )));
        }
        let version = self.load_version(&inst.pinned_def_version_id).await?;
        // send / edit / skip all advance the cursor identically — the resolution's signal
        // (the "why" of a skip) is captured as `followup_feedback` by the host's record path.
        let (status, next_index, next_due) =
            advance_after_review(&version.content, inst.anchor_at, inst.current_step_index);
        self.instances
            .update_state(&instance, status, next_index, next_due)
            .await?;
        Ok(())
    }
}
