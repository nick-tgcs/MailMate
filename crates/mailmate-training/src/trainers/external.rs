//! A subprocess-shaped [`TrainerBackend`] that drives an external toolchain — the proven
//! LoRA fallback when on-device LoRA is wanted before Burn can do it.
//!
//! The actual process invocation is behind an injectable [`CommandRunner`], so this crate
//! stays backend-free (no `std::process` in the domain layer) and the job-serialization +
//! result-handling are tested deterministically without spawning anything. A real runner
//! would write the spec to a file, invoke e.g. a `peft`-based trainer, and read back the
//! produced artifact's path and hashes.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use mailmate_common::error::TrainerError;
use mailmate_common::ids::TrainerId;
use mailmate_common::training::{AdapterFormat, AdapterType};

use crate::trainer::{
    TrainedArtifact, TrainedArtifactKind, TrainerBackend, TrainerCapabilities, TrainingJob,
};

/// The job, serialized to the spec an external trainer consumes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExternalTrainSpec {
    /// The dataset id.
    pub dataset_id: String,
    /// The objective (`sft` / `preference`).
    pub objective: String,
    /// Whether a LoRA adapter is requested.
    pub produce_lora: bool,
    /// The target base-model family.
    pub base_model_family: String,
    /// The exact base model.
    pub base_model_name: String,
    /// How many examples are in the job.
    pub example_count: usize,
    /// The training data, rendered as newline-delimited JSON.
    pub examples_jsonl: String,
}

/// The artifact metadata an external trainer reports back.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExternalTrainResult {
    /// Where the produced artifact was written.
    pub artifact_path: String,
    /// The artifact's weight format.
    pub format: AdapterFormat,
    /// The tokenizer hash, when reported.
    pub tokenizer_hash: Option<String>,
    /// The chat-template hash, when reported.
    pub chat_template_hash: Option<String>,
}

/// The seam where a real backend spawns the external tool. Injecting it keeps `std::process`
/// out of this crate and makes the backend testable with a stub.
pub trait CommandRunner: Send + Sync {
    /// Run the external trainer over `spec`, returning the produced artifact's metadata.
    ///
    /// # Errors
    /// [`TrainerError::Backend`] if the tool fails.
    fn run(&self, spec: &ExternalTrainSpec) -> Result<ExternalTrainResult, TrainerError>;
}

/// An external-toolchain trainer. Its capabilities are configured at construction, because
/// what the external tool can do (e.g. whether it does LoRA) is a property of the tool.
pub struct ExternalTrainer {
    id: TrainerId,
    capabilities: TrainerCapabilities,
    runner: std::sync::Arc<dyn CommandRunner>,
}

impl ExternalTrainer {
    /// Build an external trainer over `runner` with the given `capabilities`.
    #[must_use]
    pub fn new(
        capabilities: TrainerCapabilities,
        runner: std::sync::Arc<dyn CommandRunner>,
    ) -> Self {
        Self {
            id: TrainerId::from("trainer_external"),
            capabilities,
            runner,
        }
    }

    fn spec_for(job: &TrainingJob) -> Result<ExternalTrainSpec, TrainerError> {
        let examples_jsonl = job
            .examples
            .iter()
            .map(|e| {
                serde_json::to_string(e).map_err(|err| TrainerError::InvalidJob(err.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");
        Ok(ExternalTrainSpec {
            dataset_id: job.dataset_id.as_str().to_owned(),
            objective: job.objective.as_str().to_owned(),
            produce_lora: job.produce_lora,
            base_model_family: job.base_model_family.clone(),
            base_model_name: job.base_model_name.clone(),
            example_count: job.examples.len(),
            examples_jsonl,
        })
    }
}

#[async_trait]
impl TrainerBackend for ExternalTrainer {
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
                "external trainer cannot run objective {:?} (produce_lora={})",
                job.objective, job.produce_lora
            )));
        }
        let spec = Self::spec_for(&job)?;
        let result = self.runner.run(&spec)?;
        // A LoRA job yields a real adapter descriptor (the runner's format + a LoRA type); a
        // small-model job is honestly adapter-less.
        let (kind, format, adapter_type) = if job.produce_lora {
            (
                TrainedArtifactKind::LoraAdapter,
                Some(result.format),
                Some(AdapterType::Lora),
            )
        } else {
            (TrainedArtifactKind::SmallModel, None, None)
        };
        Ok(TrainedArtifact {
            kind,
            artifact_path: result.artifact_path,
            format,
            adapter_type,
            base_model_family: job.base_model_family,
            base_model_name: job.base_model_name,
            tokenizer_hash: result.tokenizer_hash,
            chat_template_hash: result.chat_template_hash,
            example_count: job.examples.len(),
            training_metrics: std::collections::BTreeMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use std::sync::{Arc, Mutex};

    use mailmate_common::evidence::EvidenceSourceKind;
    use mailmate_common::feedback::FeedbackPolarity;
    use mailmate_common::ids::{DatasetId, FeedbackId};
    use mailmate_common::time::Timestamp;
    use mailmate_common::training::{
        CandidateOutput, ContextFeatures, ExportPrivacyLevel, SourceFeedbackRef, TrainingExample,
        TrainingInput, TrainingLabel, TrainingObjective, TrainingTask,
    };

    use crate::trainer::TrainingHyperparams;

    /// A runner that records the spec it saw and returns a canned result (or an error).
    struct StubRunner {
        seen: Mutex<Option<ExternalTrainSpec>>,
        result: Result<ExternalTrainResult, TrainerError>,
    }

    impl CommandRunner for StubRunner {
        fn run(&self, spec: &ExternalTrainSpec) -> Result<ExternalTrainResult, TrainerError> {
            *self.seen.lock().unwrap() = Some(spec.clone());
            self.result.clone()
        }
    }

    fn lora_caps() -> TrainerCapabilities {
        TrainerCapabilities {
            sft: true,
            preference: true,
            lora: true,
            on_device: false,
        }
    }

    fn job(examples: usize) -> TrainingJob {
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
            produce_lora: true,
            examples: vec![ex; examples],
            base_model_family: "llama".to_owned(),
            base_model_name: "llama-3".to_owned(),
            hyperparams: TrainingHyperparams::default(),
        }
    }

    #[test]
    fn serializes_the_job_and_maps_the_runner_result() {
        let runner = Arc::new(StubRunner {
            seen: Mutex::new(None),
            result: Ok(ExternalTrainResult {
                artifact_path: "/ext/out.safetensors".to_owned(),
                format: AdapterFormat::Safetensors,
                tokenizer_hash: Some("tok_ext".to_owned()),
                chat_template_hash: None,
            }),
        });
        let trainer = ExternalTrainer::new(lora_caps(), runner.clone());
        let artifact = block_on(trainer.train(job(2))).unwrap();
        assert_eq!(artifact.artifact_path, "/ext/out.safetensors");
        assert_eq!(artifact.tokenizer_hash.as_deref(), Some("tok_ext"));
        // The runner saw a spec carrying the rendered examples.
        let spec = runner.seen.lock().unwrap().clone().unwrap();
        assert_eq!(spec.example_count, 2);
        assert_eq!(spec.objective, "sft");
        assert!(spec.examples_jsonl.contains("draft_reply"));
    }

    #[test]
    fn surfaces_a_runner_failure_as_a_backend_error() {
        let runner = Arc::new(StubRunner {
            seen: Mutex::new(None),
            result: Err(TrainerError::Backend("peft exited 1".to_owned())),
        });
        let trainer = ExternalTrainer::new(lora_caps(), runner);
        let err = block_on(trainer.train(job(1))).unwrap_err();
        assert!(matches!(err, TrainerError::Backend(_)));
    }

    #[test]
    fn rejects_a_lora_job_when_not_lora_capable() {
        let runner = Arc::new(StubRunner {
            seen: Mutex::new(None),
            result: Ok(ExternalTrainResult {
                artifact_path: "/x".to_owned(),
                format: AdapterFormat::Safetensors,
                tokenizer_hash: None,
                chat_template_hash: None,
            }),
        });
        let sft_only = TrainerCapabilities {
            sft: true,
            preference: false,
            lora: false,
            on_device: false,
        };
        let trainer = ExternalTrainer::new(sft_only, runner);
        let err = block_on(trainer.train(job(1))).unwrap_err();
        assert!(matches!(err, TrainerError::Unsupported(_)));
    }
}
