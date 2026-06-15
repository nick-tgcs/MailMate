//! The on-device training layer: deriving datasets from the per-task feedback tables,
//! redacting and gating them on privacy, exporting the four JSONL views, importing and
//! checking adapter compatibility, evaluating a candidate, and gating its promotion — all
//! above one swappable [`TrainerBackend`].
//!
//! This crate is the trait-OWNER of the training subsystem and is itself **backend-free**:
//! it names `mailmate-common` + `mailmate-ports` and nothing else. The in-process Burn
//! trainer lives in `mailmate-ml` and depends on *this* crate's [`TrainerBackend`]; the
//! mock and external (subprocess) trainers ship here, so no test needs a GPU or an external
//! tool. Nothing here can activate a rule or weaken a policy — an adapter is advisory, and
//! reaches `active` only by passing the evaluation gate.

pub mod datasets;
pub mod evaluation;
pub mod examples;
pub mod export;
pub mod lora;
pub mod pipeline;
pub mod privacy;
pub mod source;
pub mod trainer;
pub mod trainers;

pub use datasets::{build_dataset, DatasetPlan};
pub use evaluation::{evaluate_adapter, evaluate_promotion};
pub use lora::{check_compatibility, import_adapter, register_local_adapter, spec_for};
pub use pipeline::DefaultTrainingPipeline;
pub use privacy::{assert_ceiling_allowed, detect_safety_flags, enforce_privacy, redact_text};
pub use source::{FeedbackTrainingSource, TrainingDataSource};
pub use trainer::{
    TrainedArtifact, TrainedArtifactKind, TrainerBackend, TrainerCapabilities, TrainingHyperparams,
    TrainingJob,
};

/// Returns this crate's package name for smoke tests.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_available() {
        assert_eq!(super::crate_name(), "mailmate-training");
    }
}
