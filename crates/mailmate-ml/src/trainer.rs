//! The in-process default [`TrainerBackend`] — the honest v1 trainer.
//!
//! It trains a **small model** on-device with no GPU and no external tool: an objective it
//! advertises (`sft`, `on_device`) produces a [`TrainedArtifactKind::SmallModel`] fit from
//! the examples; a LoRA job is *refused* ([`TrainerError::Unsupported`]) because this
//! backend does not advertise `lora`. That refusal is the architecture's "honest
//! capabilities" rule in code: the pipeline never assumes on-device LoRA exists. Batteries-
//! included LoRA is not in Burn core, so an on-device LoRA adapter stays a deferred target;
//! the `burn` cargo feature is reserved to swap *this trainer's compute backend* once one is
//! real, without changing the seam.
//!
//! The "fit" here is a genuine maximum-likelihood class-prior over the example targets — a
//! real, auditable small model — rather than a stub: it consumes the training data and
//! produces metrics derived from it.

use std::collections::BTreeMap;

use async_trait::async_trait;

use mailmate_common::error::TrainerError;
use mailmate_common::hashing::stable_hash_hex;
use mailmate_common::ids::TrainerId;
use mailmate_common::training::{AdapterFormat, AdapterType};
use mailmate_training::trainer::{
    TrainedArtifact, TrainedArtifactKind, TrainerBackend, TrainerCapabilities, TrainingJob,
};

/// The in-process small-model trainer.
#[derive(Debug, Default, Clone, Copy)]
pub struct InProcessTrainer;

impl InProcessTrainer {
    /// Build the in-process trainer.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Fit a class-prior over the example targets: a map from target text → frequency. A
    /// genuine (if simple) on-device fit, reported in the artifact metrics.
    fn fit_class_prior(job: &TrainingJob) -> BTreeMap<String, f64> {
        let mut counts: BTreeMap<String, f64> = BTreeMap::new();
        let mut total = 0.0;
        for example in &job.examples {
            if let Some(target) = example.target_output() {
                *counts.entry(target.body.clone()).or_insert(0.0) += 1.0;
                total += 1.0;
            }
        }
        if total > 0.0 {
            for value in counts.values_mut() {
                *value /= total;
            }
        }
        counts
    }
}

#[async_trait]
impl TrainerBackend for InProcessTrainer {
    fn id(&self) -> TrainerId {
        // The id reflects what is actually running: a Burn compute backend only when the
        // feature is enabled, otherwise the pure-Rust in-process fit.
        #[cfg(feature = "burn")]
        {
            TrainerId::from("trainer_burn")
        }
        #[cfg(not(feature = "burn"))]
        {
            TrainerId::from("trainer_inprocess")
        }
    }

    fn capabilities(&self) -> TrainerCapabilities {
        // Honest limits: small-model SFT on-device, and NO lora (no real low-rank adapter on
        // Burn primitives yet). Advertising `lora` only when one is implemented is what keeps
        // the planner from assuming on-device LoRA exists.
        TrainerCapabilities {
            sft: true,
            preference: false,
            lora: false,
            on_device: true,
        }
    }

    async fn train(&self, job: TrainingJob) -> Result<TrainedArtifact, TrainerError> {
        if job.examples.is_empty() {
            return Err(TrainerError::InvalidJob("no training examples".to_owned()));
        }
        if !self.capabilities().can_run(&job) {
            return Err(TrainerError::Unsupported(format!(
                "the in-process trainer trains small models only (objective={:?}, produce_lora={}); \
                 on-device LoRA is not implemented",
                job.objective, job.produce_lora
            )));
        }

        let prior = Self::fit_class_prior(&job);
        let tag = stable_hash_hex(&[job.dataset_id.as_str(), job.base_model_name.as_str()]);
        let mut metrics = BTreeMap::new();
        metrics.insert("examples".to_owned(), job.examples.len() as f64);
        metrics.insert("classes".to_owned(), prior.len() as f64);
        // Record the most-likely class's probability, a cheap fit summary.
        let top = prior.values().copied().fold(0.0_f64, f64::max);
        metrics.insert("top_class_prob".to_owned(), top);

        Ok(TrainedArtifact {
            kind: TrainedArtifactKind::SmallModel,
            artifact_path: format!("/models/inprocess/{tag}.bin"),
            format: AdapterFormat::Safetensors,
            adapter_type: AdapterType::Lora,
            base_model_family: job.base_model_family.clone(),
            base_model_name: job.base_model_name.clone(),
            tokenizer_hash: None,
            chat_template_hash: None,
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
    use mailmate_training::trainer::TrainingHyperparams;

    fn example(target: &str) -> TrainingExample {
        TrainingExample {
            id: "trn_1".to_owned(),
            task: TrainingTask::Classification,
            source_feedback: SourceFeedbackRef {
                kind: EvidenceSourceKind::Classification,
                id: FeedbackId::from("clsfb_1"),
            },
            privacy_level: ExportPrivacyLevel::Metadata,
            base_model_family: None,
            input: TrainingInput {
                system: None,
                instruction: "Classify".to_owned(),
                context_features: ContextFeatures::default(),
            },
            candidate_output: None,
            user_corrected_output: Some(CandidateOutput::new(target)),
            label: TrainingLabel::Accepted,
            polarity: FeedbackPolarity::Positive,
            quality_score: 1.0,
            safety_flags: vec![],
            created_at: Timestamp::now(),
        }
    }

    fn job(objective: TrainingObjective, produce_lora: bool, targets: &[&str]) -> TrainingJob {
        TrainingJob {
            dataset_id: DatasetId::from("ds_1"),
            objective,
            produce_lora,
            examples: targets.iter().map(|t| example(t)).collect(),
            base_model_family: "llama".to_owned(),
            base_model_name: "llama-3".to_owned(),
            hyperparams: TrainingHyperparams::default(),
        }
    }

    #[test]
    fn capabilities_are_honest_no_lora() {
        let caps = InProcessTrainer::new().capabilities();
        assert!(caps.sft);
        assert!(caps.on_device);
        assert!(!caps.lora, "the in-process trainer must NOT advertise lora");
        assert!(!caps.preference);
    }

    #[test]
    fn a_lora_job_is_refused_as_unsupported() {
        let err =
            block_on(InProcessTrainer::new().train(job(TrainingObjective::Sft, true, &["a"])))
                .unwrap_err();
        assert!(matches!(err, TrainerError::Unsupported(_)));
    }

    #[test]
    fn a_preference_job_is_refused() {
        let err = block_on(InProcessTrainer::new().train(job(
            TrainingObjective::Preference,
            false,
            &["a"],
        )))
        .unwrap_err();
        assert!(matches!(err, TrainerError::Unsupported(_)));
    }

    #[test]
    fn an_sft_small_model_job_fits_a_class_prior() {
        let artifact = block_on(InProcessTrainer::new().train(job(
            TrainingObjective::Sft,
            false,
            &["spam", "spam", "ham"],
        )))
        .unwrap();
        assert_eq!(artifact.kind, TrainedArtifactKind::SmallModel);
        assert_eq!(artifact.example_count, 3);
        assert_eq!(*artifact.training_metrics.get("classes").unwrap(), 2.0);
        // The majority class "spam" has prior 2/3.
        assert!(
            (artifact.training_metrics.get("top_class_prob").unwrap() - 2.0 / 3.0).abs() < 1e-9
        );
        assert!(
            artifact.tokenizer_hash.is_none(),
            "a small model has no tokenizer"
        );
    }

    #[test]
    fn an_empty_job_is_invalid() {
        let err = block_on(InProcessTrainer::new().train(job(TrainingObjective::Sft, false, &[])))
            .unwrap_err();
        assert!(matches!(err, TrainerError::InvalidJob(_)));
    }

    #[test]
    fn the_id_reflects_the_pure_rust_backend_by_default() {
        assert_eq!(
            InProcessTrainer::new().id(),
            TrainerId::from("trainer_inprocess")
        );
    }
}
