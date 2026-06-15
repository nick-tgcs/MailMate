//! The `TrainerBackend` trait — the one seam the weight-crunching sits behind — and the
//! job / artifact / capability types it speaks.
//!
//! This crate **owns** the trait. The default in-process Burn impl lives in `mailmate-ml`
//! and depends on this crate; a subprocess `ExternalTrainer` and an in-memory `MockTrainer`
//! ship under [`crate::trainers`]. MailMate drives the whole pipeline; only `train` is
//! behind the backend.
//!
//! [`TrainerCapabilities`] is how the honest Burn limits surface in code rather than prose:
//! a backend advertises `lora` only once a real low-rank adapter is implemented, so the
//! pipeline refuses a LoRA job on a backend that cannot produce one ([`TrainerError::Unsupported`])
//! instead of silently assuming on-device LoRA exists.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use mailmate_common::error::TrainerError;
use mailmate_common::ids::{DatasetId, TrainerId};
use mailmate_common::training::{AdapterFormat, AdapterType, TrainingExample, TrainingObjective};

/// What a backend can do. The pipeline checks this *before* training, so an honest backend
/// that cannot produce a LoRA never has a LoRA job forced on it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrainerCapabilities {
    /// Supervised fine-tuning of a small model.
    pub sft: bool,
    /// Preference optimization.
    pub preference: bool,
    /// Producing a portable low-rank (LoRA) adapter.
    pub lora: bool,
    /// Runs locally with no GPU/external requirement.
    pub on_device: bool,
}

impl TrainerCapabilities {
    /// Whether this backend can satisfy `job`: it must support the objective and, when the
    /// job asks for a LoRA, advertise `lora`.
    #[must_use]
    pub fn can_run(&self, job: &TrainingJob) -> bool {
        let objective_ok = match job.objective {
            TrainingObjective::Sft => self.sft,
            TrainingObjective::Preference => self.preference,
        };
        objective_ok && (!job.produce_lora || self.lora)
    }
}

/// Knobs for a training run. Backend-neutral; a backend ignores any it does not use.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct TrainingHyperparams {
    /// Number of passes over the data.
    pub epochs: usize,
    /// Learning rate.
    pub learning_rate: f64,
    /// A fixed seed, so a run is reproducible.
    pub seed: u64,
}

impl Default for TrainingHyperparams {
    fn default() -> Self {
        Self {
            epochs: 3,
            learning_rate: 1e-4,
            seed: 0,
        }
    }
}

/// A self-contained training job: the (already privacy-enforced) examples plus everything a
/// backend needs to produce and label an artifact.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TrainingJob {
    /// The dataset these examples were drawn from.
    pub dataset_id: DatasetId,
    /// What to optimize.
    pub objective: TrainingObjective,
    /// Whether to produce a portable LoRA adapter (vs. a small model).
    pub produce_lora: bool,
    /// The training examples (already redacted to the export's privacy ceiling).
    pub examples: Vec<TrainingExample>,
    /// The target base-model family.
    pub base_model_family: String,
    /// The exact target base model.
    pub base_model_name: String,
    /// Training knobs.
    pub hyperparams: TrainingHyperparams,
}

/// What kind of artifact a run produced.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrainedArtifactKind {
    /// A small discriminative/generative model (the honest v1 default).
    SmallModel,
    /// A portable low-rank adapter.
    LoraAdapter,
}

/// The output of a successful run: where the weights landed plus the metadata needed to
/// register and (for a LoRA) check the compatibility of the artifact.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TrainedArtifact {
    /// The kind of artifact.
    pub kind: TrainedArtifactKind,
    /// Where the weights were written.
    pub artifact_path: String,
    /// The on-disk weight format.
    pub format: AdapterFormat,
    /// The adapter kind (for a LoRA; `Lora` for a small model is meaningless but harmless).
    pub adapter_type: AdapterType,
    /// The base-model family the artifact is compatible with.
    pub base_model_family: String,
    /// The exact base model it was trained against.
    pub base_model_name: String,
    /// The tokenizer hash, when the backend can compute one.
    pub tokenizer_hash: Option<String>,
    /// The chat-template hash, when the backend can compute one.
    pub chat_template_hash: Option<String>,
    /// How many examples it trained on.
    pub example_count: usize,
    /// Backend training metrics (loss, steps, …), for the audit trail.
    pub training_metrics: BTreeMap<String, f64>,
}

/// The swappable weight-crunching seam. Everything *above* `train` — deriving examples,
/// redaction, dataset assembly, evaluation, and the promotion gate — is MailMate's and
/// lives in this crate; only `train` is behind the backend.
#[async_trait]
pub trait TrainerBackend: Send + Sync {
    /// This backend's identity.
    fn id(&self) -> TrainerId;

    /// What this backend can do (objectives, LoRA, on-device).
    fn capabilities(&self) -> TrainerCapabilities;

    /// Run `job`, returning the produced artifact.
    ///
    /// # Errors
    /// [`TrainerError::Unsupported`] if the job needs a capability this backend does not
    /// advertise; [`TrainerError::InvalidJob`] if the job is malformed (e.g. no examples);
    /// [`TrainerError::Backend`] on a compute/tool failure.
    async fn train(&self, job: TrainingJob) -> Result<TrainedArtifact, TrainerError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::evidence::EvidenceSourceKind;
    use mailmate_common::feedback::FeedbackPolarity;
    use mailmate_common::ids::FeedbackId;
    use mailmate_common::time::Timestamp;
    use mailmate_common::training::{
        CandidateOutput, ContextFeatures, ExportPrivacyLevel, SourceFeedbackRef, TrainingInput,
        TrainingLabel, TrainingTask,
    };

    fn a_job(objective: TrainingObjective, produce_lora: bool) -> TrainingJob {
        TrainingJob {
            dataset_id: DatasetId::from("ds_1"),
            objective,
            produce_lora,
            examples: vec![TrainingExample {
                id: "trn_1".to_owned(),
                task: TrainingTask::DraftReply,
                source_feedback: SourceFeedbackRef {
                    kind: EvidenceSourceKind::Draft,
                    id: FeedbackId::from("drffb_1"),
                },
                privacy_level: ExportPrivacyLevel::Redacted,
                base_model_family: Some("llama".to_owned()),
                input: TrainingInput {
                    system: None,
                    instruction: "Draft a reply".to_owned(),
                    context_features: ContextFeatures::default(),
                },
                candidate_output: Some(CandidateOutput::new("hi")),
                user_corrected_output: None,
                label: TrainingLabel::Accepted,
                polarity: FeedbackPolarity::Positive,
                quality_score: 0.9,
                safety_flags: vec![],
                created_at: Timestamp::now(),
            }],
            base_model_family: "llama".to_owned(),
            base_model_name: "llama-3".to_owned(),
            hyperparams: TrainingHyperparams::default(),
        }
    }

    #[test]
    fn capabilities_gate_objective_and_lora() {
        let sft_only = TrainerCapabilities {
            sft: true,
            preference: false,
            lora: false,
            on_device: true,
        };
        // An SFT small-model job runs.
        assert!(sft_only.can_run(&a_job(TrainingObjective::Sft, false)));
        // A LoRA job does NOT run on a backend that does not advertise lora.
        assert!(!sft_only.can_run(&a_job(TrainingObjective::Sft, true)));
        // A preference job does not run on an sft-only backend.
        assert!(!sft_only.can_run(&a_job(TrainingObjective::Preference, false)));

        let lora_capable = TrainerCapabilities {
            sft: true,
            preference: true,
            lora: true,
            on_device: false,
        };
        assert!(lora_capable.can_run(&a_job(TrainingObjective::Sft, true)));
        assert!(lora_capable.can_run(&a_job(TrainingObjective::Preference, false)));
    }

    #[test]
    fn default_hyperparams_are_sane() {
        let hp = TrainingHyperparams::default();
        assert_eq!(hp.epochs, 3);
        assert!(hp.learning_rate > 0.0);
        assert_eq!(hp.seed, 0);
    }
}
