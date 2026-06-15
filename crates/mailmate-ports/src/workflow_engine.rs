//! The workflow-engine port: arm a pipeline item on a workflow, detect containment
//! conflicts, and apply the user's control actions (reschedule/snooze, resolve a surfaced
//! review). It **drives** the existing planner/drafter — it does not plan, guard, or send.
//!
//! A workflow *conflict* is the containment check "don't run two active workflows on one
//! item" — structurally unlike the AST/effect overlap a rule conflict records, so it has
//! its own [`WorkflowConflict`] type and owner table.

use async_trait::async_trait;

use mailmate_common::error::WorkflowError;
use mailmate_common::ids::{PipelineItemId, WorkflowDefId, WorkflowInstanceId};
use mailmate_common::time::Timestamp;
use mailmate_common::workflow::{ReviewResolution, WorkflowConflict};

/// Arms and steers follow-up workflow instances.
#[async_trait]
pub trait WorkflowEngine: Send + Sync {
    /// Arm `item` on `workflow`: pin the definition's current version, resolve the anchor,
    /// compute the first `next_due_at`, and create an `active` instance. Returns its id.
    ///
    /// # Errors
    /// [`WorkflowError::NotFound`] if the item or workflow is absent, or
    /// [`WorkflowError::Storage`] on a persistence failure.
    async fn arm(
        &self,
        item: PipelineItemId,
        workflow: WorkflowDefId,
    ) -> Result<WorkflowInstanceId, WorkflowError>;

    /// Detect containment conflicts: another active instance already chases `item`. The
    /// returned conflicts are **not** persisted by this call — the caller decides.
    ///
    /// # Errors
    /// [`WorkflowError::Storage`] on a read failure.
    async fn detect_conflicts(
        &self,
        item: &PipelineItemId,
        workflow: &WorkflowDefId,
    ) -> Result<Vec<WorkflowConflict>, WorkflowError>;

    /// Push an instance's next step out (`reschedule`/`snooze`). With `snooze = true` the
    /// instance moves to `snoozed`; either way `next_due_at` is set to `next_due_at`.
    ///
    /// # Errors
    /// [`WorkflowError::NotFound`] if the instance is absent, or [`WorkflowError::Storage`]
    /// on a persistence failure.
    async fn reschedule(
        &self,
        instance: WorkflowInstanceId,
        next_due_at: Timestamp,
        snooze: bool,
    ) -> Result<(), WorkflowError>;

    /// Resolve a surfaced follow-up draft (`review_followup`): advance the cursor past the
    /// surfaced step and either re-arm the instance (more steps remain) or complete it. No
    /// resolution sends automatically — `send` means the human dispatched the draft.
    ///
    /// # Errors
    /// [`WorkflowError::NotFound`] if the instance is absent,
    /// [`WorkflowError::InvalidState`] if it is not awaiting review, or
    /// [`WorkflowError::Storage`] on a persistence failure.
    async fn resolve_review(
        &self,
        instance: WorkflowInstanceId,
        resolution: ReviewResolution,
        now: Timestamp,
    ) -> Result<(), WorkflowError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn WorkflowEngine) {}
        let _ = takes as fn(&dyn WorkflowEngine);
    }
}
