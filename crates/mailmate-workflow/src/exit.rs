//! [`DefaultExitDetector`]: a reply (matched by host-side thread identity) or a won/lost/
//! cancel deal event exits every non-terminal sequence on a pipeline item, clearing
//! `next_due_at` so the scheduler stops selecting the row, and advancing the deal's stage
//! where the event implies one.

use std::sync::Arc;

use async_trait::async_trait;

use mailmate_common::error::WorkflowError;
use mailmate_common::ids::{PipelineItemId, WorkflowInstanceId};
use mailmate_common::workflow::ExitEvent;
use mailmate_ports::exit_detector::ExitDetector;
use mailmate_ports::storage::pipeline_items::PipelineItemRepository;
use mailmate_ports::storage::workflows::WorkflowInstanceRepository;

/// The default exit detector over the instance + item repository ports.
#[derive(Clone)]
pub struct DefaultExitDetector {
    instances: Arc<dyn WorkflowInstanceRepository>,
    items: Arc<dyn PipelineItemRepository>,
}

impl DefaultExitDetector {
    /// Assemble the detector from its ports.
    #[must_use]
    pub fn new(
        instances: Arc<dyn WorkflowInstanceRepository>,
        items: Arc<dyn PipelineItemRepository>,
    ) -> Self {
        Self { instances, items }
    }
}

#[async_trait]
impl ExitDetector for DefaultExitDetector {
    async fn on_exit_event(
        &self,
        item: PipelineItemId,
        event: ExitEvent,
    ) -> Result<Vec<WorkflowInstanceId>, WorkflowError> {
        let new_status = event.resulting_status();
        let mut exited = Vec::new();
        for inst in self.instances.list_by_pipeline_item(&item).await? {
            if inst.status.is_terminal() {
                continue;
            }
            // Exit always clears next_due_at: the scheduler simply stops selecting the row.
            self.instances
                .update_state(&inst.id, new_status, inst.current_step_index, None)
                .await?;
            exited.push(inst.id);
        }
        if let Some(stage) = event.resulting_stage() {
            self.items.update_stage(&item, stage).await?;
        }
        Ok(exited)
    }
}
