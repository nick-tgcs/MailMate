//! The training use-case: a thin orchestration seam over the training-pipeline port.
//!
//! [`TrainingService`] is how the core drives the on-device training loop — derive a dataset
//! from the captured feedback, train a candidate adapter, evaluate it, and gate its
//! promotion. It names only the port, so the concrete pipeline
//! (`mailmate-training::DefaultTrainingPipeline`) and its swappable trainer backend are
//! injected at the edge. Nothing here can activate a rule or weaken a policy: an adapter is
//! advisory and reaches `active` only by passing the evaluation gate inside the pipeline.

use std::sync::Arc;

use mailmate_common::error::TrainingError;
use mailmate_common::training::{TrainingPipelineReport, TrainingPipelineRequest};
use mailmate_ports::training_pipeline::TrainingPipeline;

use crate::Ports;

/// Runs the on-device training pipeline.
#[derive(Clone)]
pub struct TrainingService {
    pipeline: Arc<dyn TrainingPipeline>,
}

impl TrainingService {
    /// Assemble the service from the training-pipeline port.
    #[must_use]
    pub fn new(pipeline: Arc<dyn TrainingPipeline>) -> Self {
        Self { pipeline }
    }

    /// Assemble the service from the core's [`Ports`] bundle.
    #[must_use]
    pub fn from_ports(ports: &Ports) -> Self {
        Self::new(ports.training_pipeline.clone())
    }

    /// Run one training pipeline pass, returning the datasets built, the candidate adapter
    /// (with its post-gate status), the evaluation run, and the gate's verdict. A candidate
    /// that fails the gate is recorded `failed_eval`, never promoted.
    ///
    /// # Errors
    /// [`TrainingError`] if there are no eligible examples, a privacy ceiling is violated, the
    /// trainer lacks a required capability, evaluation fails, or a backend failure occurs.
    pub async fn run(
        &self,
        request: TrainingPipelineRequest,
    ) -> Result<TrainingPipelineReport, TrainingError> {
        self.pipeline.run(request).await
    }
}
