//! The exit-detector port: a reply (matched by host-side thread identity) or a won/lost/
//! cancel deal event exits the running sequences on a pipeline item. Exiting always clears
//! `next_due_at`, so the scheduler simply stops selecting the row.

use async_trait::async_trait;

use mailmate_common::error::WorkflowError;
use mailmate_common::ids::{PipelineItemId, WorkflowInstanceId};
use mailmate_common::workflow::ExitEvent;

/// Exits running follow-up sequences on inbound replies and deal events.
#[async_trait]
pub trait ExitDetector: Send + Sync {
    /// Apply `event` to every non-terminal instance chasing `item`: a reply → `engaged`,
    /// won/lost → `completed`, cancel → `cancelled`; and update the pipeline stage where the
    /// event implies one. Returns the instances that were exited.
    ///
    /// # Errors
    /// [`WorkflowError::Storage`] on a persistence failure.
    async fn on_exit_event(
        &self,
        item: PipelineItemId,
        event: ExitEvent,
    ) -> Result<Vec<WorkflowInstanceId>, WorkflowError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn ExitDetector) {}
        let _ = takes as fn(&dyn ExitDetector);
    }
}
