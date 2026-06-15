//! The training-pipeline port: the one entry point the core drives to turn captured
//! feedback into an evaluated, gated adapter.
//!
//! MailMate *drives* the whole loop — derive a dataset from the per-task feedback tables,
//! train a candidate through the swappable `TrainerBackend`, evaluate it, and gate its
//! promotion — and only the weight-crunching sits behind the backend. The port returns a
//! [`TrainingPipelineReport`] whose adapter status already reflects the gate: a candidate
//! that fails evaluation is recorded `failed_eval`, never `active`. Nothing here can
//! activate a rule or weaken a policy; an adapter is advisory and its outputs still flow
//! through validation → rules → policy like any other provider's.
//!
//! The default adapter (`mailmate-training::DefaultTrainingPipeline`) composes the trainer
//! backend, the dataset/adapter/eval storage ports, and the AI provider; the core names
//! only this trait.

use async_trait::async_trait;

use mailmate_common::error::TrainingError;
use mailmate_common::training::{TrainingPipelineReport, TrainingPipelineRequest};

/// Runs the end-to-end training pipeline.
#[async_trait]
pub trait TrainingPipeline: Send + Sync {
    /// Derive a dataset for the requested tasks, train a candidate adapter, evaluate it, and
    /// gate its promotion — returning everything the run produced. A candidate is only ever
    /// promoted to `active` when it clears the request's [`PromotionPolicy`]; otherwise it is
    /// recorded `failed_eval`.
    ///
    /// [`PromotionPolicy`]: mailmate_common::training::PromotionPolicy
    ///
    /// # Errors
    /// [`TrainingError::Export`] if no eligible examples exist or a privacy ceiling is
    /// violated; [`TrainingError::Trainer`] if the backend fails or lacks a required
    /// capability; [`TrainingError::Evaluation`]/[`TrainingError::Provider`] if evaluation
    /// fails; [`TrainingError::Storage`] on a backend failure.
    async fn run(
        &self,
        request: TrainingPipelineRequest,
    ) -> Result<TrainingPipelineReport, TrainingError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn TrainingPipeline) {}
        let _ = takes as fn(&dyn TrainingPipeline);
    }
}
