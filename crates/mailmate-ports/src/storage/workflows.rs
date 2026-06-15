//! The workflow repository ports: the versioned-cadence definition store (mirroring
//! [`RuleRepository`](super::rules::RuleRepository)), the mutable-state instance store (the
//! durable temporal trigger), and the containment-conflict and shadow-outcome stores.
//!
//! A `WorkflowDefinition` reuses the rule version-immutability and lifecycle status set; a
//! `WorkflowInstance` is the single mutable-state row, polled by the follow-up scheduler via
//! its `(status, next_due_at)` index.

use async_trait::async_trait;

use mailmate_common::error::StorageError;
use mailmate_common::ids::{
    PipelineItemId, ThreadId, WorkflowConflictId, WorkflowDefId, WorkflowDefVersionId,
    WorkflowInstanceId, WorkflowShadowOutcomeId,
};
use mailmate_common::rules::rule::RuleStatus;
use mailmate_common::time::Timestamp;
use mailmate_common::workflow::{
    NewWorkflowDefVersion, NewWorkflowDefinition, NewWorkflowInstance, WorkflowConflict,
    WorkflowDefinition, WorkflowDefinitionVersion, WorkflowInstance, WorkflowInstanceStatus,
    WorkflowShadowOutcome,
};

/// Persistence for follow-up workflow definitions and their immutable versions.
#[async_trait]
pub trait WorkflowRepository: Send + Sync {
    /// Persist a new definition in [`Draft`](RuleStatus::Draft) status together with its
    /// first immutable version, atomically. Returns the new definition id.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn save_definition_draft(
        &self,
        draft: NewWorkflowDefinition,
    ) -> Result<WorkflowDefId, StorageError>;

    /// Append a new immutable version, assigning the next monotonic version number and
    /// re-pointing the definition's current version. Returns the version id.
    ///
    /// # Errors
    /// [`StorageError::Constraint`] if the definition does not exist, or other
    /// [`StorageError`] on a backend failure.
    async fn create_version(
        &self,
        version: NewWorkflowDefVersion,
    ) -> Result<WorkflowDefVersionId, StorageError>;

    /// Transition a definition to a new lifecycle status.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn update_status(
        &self,
        id: &WorkflowDefId,
        status: RuleStatus,
    ) -> Result<(), StorageError>;

    /// Fetch a definition by id, or `None` if absent.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn get_definition(
        &self,
        id: &WorkflowDefId,
    ) -> Result<Option<WorkflowDefinition>, StorageError>;

    /// Fetch an immutable version by id, or `None` if absent.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn get_version(
        &self,
        id: &WorkflowDefVersionId,
    ) -> Result<Option<WorkflowDefinitionVersion>, StorageError>;

    /// All definitions in `status` (e.g. `active` for arm, `shadow_mode` for shadow runs).
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn list_by_status(
        &self,
        status: RuleStatus,
    ) -> Result<Vec<WorkflowDefinition>, StorageError>;
}

/// Persistence for the running workflow instances — the durable temporal trigger and the
/// one mutable-state row.
#[async_trait]
pub trait WorkflowInstanceRepository: Send + Sync {
    /// Arm a new instance; returns its id. The repository stamps the id and timestamps.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn arm(&self, instance: NewWorkflowInstance) -> Result<WorkflowInstanceId, StorageError>;

    /// Fetch an instance by id, or `None` if absent.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn get(&self, id: &WorkflowInstanceId) -> Result<Option<WorkflowInstance>, StorageError>;

    /// The due instances at `now`: `status IN (active, snoozed) AND next_due_at <= now`,
    /// ordered by `next_due_at` (the scheduler drain — the load-bearing index).
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn list_due(&self, now: Timestamp) -> Result<Vec<WorkflowInstance>, StorageError>;

    /// All non-terminal instances anchored on `thread_id` (the reply-exit lookup).
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn list_active_by_thread(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Vec<WorkflowInstance>, StorageError>;

    /// All instances chasing `pipeline_item` (conflict detection, won/lost/cancel).
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn list_by_pipeline_item(
        &self,
        pipeline_item_id: &PipelineItemId,
    ) -> Result<Vec<WorkflowInstance>, StorageError>;

    /// The one mutable update: set the instance's status, cursor, and trigger. The caller
    /// is responsible for honouring the `next_due_at` non-NULL-iff-selectable invariant.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn update_state(
        &self,
        id: &WorkflowInstanceId,
        status: WorkflowInstanceStatus,
        current_step_index: i64,
        next_due_at: Option<Timestamp>,
    ) -> Result<(), StorageError>;
}

/// Persistence for recorded workflow containment conflicts (own owner table).
#[async_trait]
pub trait WorkflowConflictRepository: Send + Sync {
    /// Record one detected conflict; returns its id.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn append(&self, conflict: WorkflowConflict) -> Result<WorkflowConflictId, StorageError>;

    /// All still-open conflicts, newest first.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn list_open(&self) -> Result<Vec<WorkflowConflict>, StorageError>;

    /// Mark a conflict human-resolved.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn resolve(&self, id: &WorkflowConflictId) -> Result<(), StorageError>;
}

/// Persistence for shadow follow-up step outcomes (sole owner of shadow follow-up
/// performance; separate from `shadow_outcomes` because there is no triggering message).
#[async_trait]
pub trait WorkflowShadowOutcomeRepository: Send + Sync {
    /// Record one shadow follow-up step; returns its id.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn append(
        &self,
        row: WorkflowShadowOutcome,
    ) -> Result<WorkflowShadowOutcomeId, StorageError>;

    /// All shadow outcomes recorded for `workflow_id`, oldest first (the promotion-report
    /// substrate).
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn list_for_workflow(
        &self,
        workflow_id: &WorkflowDefId,
    ) -> Result<Vec<WorkflowShadowOutcome>, StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_are_object_safe() {
        fn defs(_: &dyn WorkflowRepository) {}
        fn instances(_: &dyn WorkflowInstanceRepository) {}
        fn conflicts(_: &dyn WorkflowConflictRepository) {}
        fn shadows(_: &dyn WorkflowShadowOutcomeRepository) {}
        let _ = defs as fn(&dyn WorkflowRepository);
        let _ = instances as fn(&dyn WorkflowInstanceRepository);
        let _ = conflicts as fn(&dyn WorkflowConflictRepository);
        let _ = shadows as fn(&dyn WorkflowShadowOutcomeRepository);
    }
}
