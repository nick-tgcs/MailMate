//! MailMate machine-learning adapters: the cascade's Tier-2 classifier.
//!
//! This crate is an edge adapter implementing the `Tier2Classifier` and `TrainerBackend`
//! ports. Phase 5 ships the always-on [`LogisticRegressionClassifier`] (online, pure Rust,
//! no model runtime); Phase 9 adds the [`InProcessTrainer`] — the in-process default
//! `TrainerBackend` (an honest small-model fit that reports `lora: false`). The Burn compute
//! backend (and a Burn `AiProvider`) stay reserved behind the `burn` cargo feature until an
//! on-device LoRA target is real, so the default build stays light. `mailmate-core` never
//! depends on this crate; it sees only the ports.

pub mod logreg;
pub mod trainer;

pub use logreg::LogisticRegressionClassifier;
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
