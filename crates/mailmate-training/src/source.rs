//! Where the pipeline gets its examples: the [`TrainingDataSource`] seam and the default
//! [`FeedbackTrainingSource`] that reads the per-task feedback repositories and derives
//! examples from them.
//!
//! This is an **internal** seam of the training subsystem (the pipeline is its only
//! consumer), not a core-facing port — so it stays out of `mailmate-ports`. The default impl
//! reads only the three feedback kinds that are live today; the body-carrying kinds slot in
//! here when they land, with no change to the pipeline above.

use std::sync::Arc;

use async_trait::async_trait;

use mailmate_common::error::ExportError;
use mailmate_common::feedback::{ClassificationFeedback, FilingFeedback, RuleProposalFeedback};
use mailmate_common::training::{TrainingExample, TrainingPipelineRequest, TrainingTask};
use mailmate_ports::storage::FeedbackRepository;

use crate::examples::{derive_classification, derive_filing, derive_rule_proposal};

/// Yields the derived training examples for a pipeline request.
#[async_trait]
pub trait TrainingDataSource: Send + Sync {
    /// Collect every derived example the request asks for.
    ///
    /// # Errors
    /// [`ExportError::Storage`] if reading a feedback table fails.
    async fn collect(
        &self,
        request: &TrainingPipelineRequest,
    ) -> Result<Vec<TrainingExample>, ExportError>;
}

/// The default source: reads the live per-task feedback tables and derives examples.
pub struct FeedbackTrainingSource {
    classification: Arc<dyn FeedbackRepository<ClassificationFeedback>>,
    filing: Arc<dyn FeedbackRepository<FilingFeedback>>,
    rule_proposal: Arc<dyn FeedbackRepository<RuleProposalFeedback>>,
}

impl FeedbackTrainingSource {
    /// Build a source over the three live feedback repositories.
    #[must_use]
    pub fn new(
        classification: Arc<dyn FeedbackRepository<ClassificationFeedback>>,
        filing: Arc<dyn FeedbackRepository<FilingFeedback>>,
        rule_proposal: Arc<dyn FeedbackRepository<RuleProposalFeedback>>,
    ) -> Self {
        Self {
            classification,
            filing,
            rule_proposal,
        }
    }
}

#[async_trait]
impl TrainingDataSource for FeedbackTrainingSource {
    async fn collect(
        &self,
        request: &TrainingPipelineRequest,
    ) -> Result<Vec<TrainingExample>, ExportError> {
        let mut examples = Vec::new();
        if request.includes(TrainingTask::Classification) {
            for row in self.classification.query(Default::default()).await? {
                examples.push(derive_classification(&row));
            }
        }
        if request.includes(TrainingTask::Filing) {
            for row in self.filing.query(Default::default()).await? {
                examples.push(derive_filing(&row));
            }
        }
        if request.includes(TrainingTask::RuleProposal) {
            for row in self.rule_proposal.query(Default::default()).await? {
                examples.push(derive_rule_proposal(&row));
            }
        }
        Ok(examples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    /// A fixed in-memory source, to confirm the seam is object-safe and drives the pipeline.
    struct FixedSource(Vec<TrainingExample>);

    #[async_trait]
    impl TrainingDataSource for FixedSource {
        async fn collect(
            &self,
            _request: &TrainingPipelineRequest,
        ) -> Result<Vec<TrainingExample>, ExportError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn the_seam_is_object_safe_and_yields_examples() {
        let source: Arc<dyn TrainingDataSource> = Arc::new(FixedSource(vec![]));
        let out = block_on(source.collect(&TrainingPipelineRequest::new("d", "p"))).unwrap();
        assert!(out.is_empty());
    }
}
