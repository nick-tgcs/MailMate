//! Pure workflow conflict detection: the **containment** check "don't run two active
//! workflows on one pipeline item". Structurally unlike a rule conflict (AST/effect
//! overlap) — it does not look at the cadence content at all, only at whether another
//! non-terminal instance already chases the item.

use mailmate_common::ids::{PipelineItemId, WorkflowConflictId, WorkflowDefId};
use mailmate_common::time::Timestamp;
use mailmate_common::workflow::{
    WorkflowConflict, WorkflowConflictKind, WorkflowConflictStatus, WorkflowInstance,
};

/// Detect containment conflicts for arming `candidate` on `item`: one `Open` conflict per
/// existing non-terminal instance already chasing the item. The returned records are NOT
/// persisted — the caller decides whether to record/surface them.
#[must_use]
pub fn detect_conflicts(
    item: &PipelineItemId,
    candidate: &WorkflowDefId,
    existing: &[WorkflowInstance],
    now: Timestamp,
) -> Vec<WorkflowConflict> {
    existing
        .iter()
        .filter(|inst| !inst.status.is_terminal())
        .map(|inst| WorkflowConflict {
            id: WorkflowConflictId::fresh(),
            pipeline_item_id: item.clone(),
            workflow_a_id: inst.id.as_str().to_owned(),
            workflow_b_id: candidate.as_str().to_owned(),
            conflict_kind: WorkflowConflictKind::ConcurrentActiveWorkflow,
            status: WorkflowConflictStatus::Open,
            detected_at: now,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::ids::ThreadId;
    use mailmate_common::ids::{WorkflowDefVersionId, WorkflowInstanceId};
    use mailmate_common::workflow::WorkflowInstanceStatus;

    fn instance(id: &str, status: WorkflowInstanceStatus) -> WorkflowInstance {
        WorkflowInstance {
            id: WorkflowInstanceId::from(id),
            pipeline_item_id: PipelineItemId::from("pli_1"),
            workflow_id: WorkflowDefId::from("wfd_a"),
            pinned_def_version_id: WorkflowDefVersionId::from("wfdv_1"),
            thread_id: ThreadId::from("thread_1"),
            anchor_at: Timestamp::now(),
            status,
            current_step_index: 0,
            next_due_at: None,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        }
    }

    #[test]
    fn an_active_instance_on_the_item_is_a_conflict() {
        let existing = vec![instance("wfi_a", WorkflowInstanceStatus::Active)];
        let conflicts = detect_conflicts(
            &PipelineItemId::from("pli_1"),
            &WorkflowDefId::from("wfd_b"),
            &existing,
            Timestamp::now(),
        );
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].workflow_a_id, "wfi_a");
        assert_eq!(conflicts[0].workflow_b_id, "wfd_b");
        assert_eq!(
            conflicts[0].conflict_kind,
            WorkflowConflictKind::ConcurrentActiveWorkflow
        );
    }

    #[test]
    fn terminal_instances_do_not_conflict() {
        let existing = vec![
            instance("wfi_done", WorkflowInstanceStatus::Completed),
            instance("wfi_gone", WorkflowInstanceStatus::Cancelled),
        ];
        let conflicts = detect_conflicts(
            &PipelineItemId::from("pli_1"),
            &WorkflowDefId::from("wfd_b"),
            &existing,
            Timestamp::now(),
        );
        assert!(conflicts.is_empty(), "closed sequences never contain");
    }
}
