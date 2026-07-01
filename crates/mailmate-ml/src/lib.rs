//! MailMate machine-learning adapters: the cascade's Tier-2 classifier.
//!
//! This crate is an edge adapter implementing the `Tier2Classifier` and `TrainerBackend`
//! ports. Phase 5 ships the always-on [`LogisticRegressionClassifier`] (online, pure Rust,
//! no model runtime); Phase 8 adds the real on-device [`tier2_burn`] classifier — a linear
//! discriminative model trained by gradient descent in **Burn** (CPU `ndarray` backend) that
//! writes a **loadable artifact** the cascade loads and predicts from, gated on held-out
//! precision. The [`InProcessTrainer`] remains the honest small-model class-prior fit for the
//! separate LLM-SFT `TrainerBackend` path. `mailmate-core` never depends on this crate; it
//! sees only the ports.
//!
//! Phase 12 adds the production [`DeterministicFeatureExtractor`] — the real, pure
//! `FeatureExtractor` adapter the cascade feeds into Tier 2 (it lives here, beside the model
//! that consumes its output).

pub mod features;
pub mod featurize;
pub mod logreg;
pub mod tier2_burn;
pub mod trainer;

pub use features::DeterministicFeatureExtractor;
pub use logreg::LogisticRegressionClassifier;
pub use tier2_burn::{
    train as train_tier2, train_eval_gate as train_tier2_eval_gate, BurnTier2Classifier,
    SwappableTier2, Tier2EvalMetrics, Tier2ModelError, Tier2TrainConfig, Tier2TrainingReport,
    TrainedTier2, DEFAULT_PRECISION_GATE,
};
pub use trainer::InProcessTrainer;

/// Returns this crate's package name for smoke tests.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_available() {
        assert_eq!(super::crate_name(), "mailmate-ml");
    }
}
