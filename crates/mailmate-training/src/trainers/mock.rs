//! A deterministic in-memory [`TrainerBackend`] for tests.
//!
//! It performs no real training: it validates the job, honours the capability gate, and
//! produces a reproducible artifact whose paths/hashes are stable functions of the dataset
//! id. Because it advertises `lora`, it lets the LoRA orchestration path (register candidate
//! → evaluate → gate → promote) be tested end-to-end with no GPU.

use std::collections::BTreeMap;

use async_trait::async_trait;

use mailmate_common::error::TrainerError;
use mailmate_common::hashing::stable_hash_hex;
use mailmate_common::ids::TrainerId;
use mailmate_common::training::{AdapterFormat, AdapterType};

use crate::trainer::{
    TrainedArtifact, TrainedArtifactKind, TrainerBackend, TrainerCapabilities, TrainingJob,
};

/// A deterministic, capability-complete mock trainer.
pub struct MockTrainer {
    id: TrainerId,
    capabilities: TrainerCapabilities,
}

impl Default for MockTrainer {
    fn default() -> Self {
        Self {
            id: TrainerId::from("trainer_mock"),
            capabilities: TrainerCapabilities {
                sft: true,
                preference: true,
                lora: true,
                on_device: true,
            },
        }
    }
}

impl MockTrainer {
    /// A mock with the default (capability-complete) profile.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A mock with a restricted capability profile (to test capability gating).
    #[must_use]
    pub fn with_capabilities(capabilities: TrainerCapabilities) -> Self {
        Self {
            id: TrainerId::from("trainer_mock"),
            capabilities,
        }
    }
}

#[async_trait]
impl TrainerBackend for MockTrainer {
    fn id(&self) -> TrainerId {
        self.id.clone()
    }

    fn capabilities(&self) -> TrainerCapabilities {
        self.capabilities
    }

    async fn train(&self, job: TrainingJob) -> Result<TrainedArtifact, TrainerError> {
        if job.examples.is_empty() {
            return Err(TrainerError::InvalidJob("no training examples".to_owned()));
        }
        if !self.capabilities.can_run(&job) {
            return Err(TrainerError::Unsupported(format!(
                "mock trainer cannot run objective {:?} (produce_lora={})",
                job.objective, job.produce_lora
            )));
        }
        let tag = stable_hash_hex(&[job.dataset_id.as_str(), job.base_model_name.as_str()]);
        let kind = if job.produce_lora {
            TrainedArtifactKind::LoraAdapter
        } else {
            TrainedArtifactKind::SmallModel
        };
        let mut metrics = BTreeMap::new();
        metrics.insert("examples".to_owned(), job.examples.len() as f64);
        metrics.insert("epochs".to_owned(), job.hyperparams.epochs as f64);
        Ok(TrainedArtifact {
            kind,
            artifact_path: format!("/mock/adapters/{tag}.safetensors"),
            format: AdapterFormat::Safetensors,
            adapter_type: AdapterType::Lora,
            base_model_family: job.base_model_family.clone(),
            base_model_name: job.base_model_name.clone(),
            tokenizer_hash: Some(format!("tok_{tag}")),
            chat_template_hash: Some(format!("tmpl_{tag}")),
            example_count: job.examples.len(),
            training_metrics: metrics,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use mailmate_common::evidence::EvidenceSourceKind;
    use mailmate_common::feedback::FeedbackPolarity;
    use mailmate_common::ids::{DatasetId, FeedbackId};
    use mailmate_common::time::Timestamp;
    use mailmate_common::training::{
        CandidateOutput, ContextFeatures, ExportPrivacyLevel, SourceFeedbackRef, TrainingExample,
        TrainingInput, TrainingLabel, TrainingObjective, TrainingTask,
    };

    use crate::trainer::TrainingHyperparams;

    fn job(produce_lora: bool, examples: usize) -> TrainingJob {
        let ex = TrainingExample {
            id: "trn_1".to_owned(),
            task: TrainingTask::DraftReply,
            source_feedback: SourceFeedbackRef {
                kind: EvidenceSourceKind::Draft,
                id: FeedbackId::from("drffb_1"),
            },
            privacy_level: ExportPrivacyLevel::Redacted,
            base_model_family: None,
            input: TrainingInput {
                system: None,
                instruction: "x".to_owned(),
                context_features: ContextFeatures::default(),
            },
            candidate_output: Some(CandidateOutput::new("hi")),
            user_corrected_output: None,
            label: TrainingLabel::Accepted,
            polarity: FeedbackPolarity::Positive,
            quality_score: 0.9,
            safety_flags: vec![],
            created_at: Timestamp::now(),
        };
        TrainingJob {
            dataset_id: DatasetId::from("ds_1"),
            objective: TrainingObjective::Sft,
            produce_lora,
            examples: vec![ex; examples],
            base_model_family: "llama".to_owned(),
            base_model_name: "llama-3".to_owned(),
            hyperparams: TrainingHyperparams::default(),
        }
    }

    #[test]
    fn produces_a_deterministic_lora_artifact() {
        let trainer = MockTrainer::new();
        let a = block_on(trainer.train(job(true, 2))).unwrap();
        let b = block_on(trainer.train(job(true, 2))).unwrap();
        assert_eq!(a, b, "deterministic");
        assert_eq!(a.kind, TrainedArtifactKind::LoraAdapter);
        assert!(a.artifact_path.ends_with(".safetensors"));
        assert!(a.tokenizer_hash.is_some());
        assert_eq!(a.example_count, 2);
    }

    #[test]
    fn rejects_an_empty_job() {
        let trainer = MockTrainer::new();
        let err = block_on(trainer.train(job(true, 0))).unwrap_err();
        assert!(matches!(err, TrainerError::InvalidJob(_)));
    }

    #[test]
    fn honours_the_capability_gate() {
        let sft_only = MockTrainer::with_capabilities(TrainerCapabilities {
            sft: true,
            preference: false,
            lora: false,
            on_device: true,
        });
        // A LoRA job is rejected as unsupported.
        let err = block_on(sft_only.train(job(true, 1))).unwrap_err();
        assert!(matches!(err, TrainerError::Unsupported(_)));
        // A small-model SFT job is fine, producing a SmallModel artifact.
        let artifact = block_on(sft_only.train(job(false, 1))).unwrap();
        assert_eq!(artifact.kind, TrainedArtifactKind::SmallModel);
    }
}
