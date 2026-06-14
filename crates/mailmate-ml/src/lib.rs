//! MailMate machine-learning adapters: the cascade's Tier-2 classifier.
//!
//! This crate is an edge adapter implementing the `Tier2Classifier` port. Phase 5 ships the
//! always-on [`LogisticRegressionClassifier`] (online, pure Rust, no model runtime). The
//! Burn-backed discriminative classifier and the in-process Burn `AiProvider` are reserved
//! behind the `burn` cargo feature and land in Phase 9 alongside the on-device trainer — a
//! *trained* Burn model has no meaning without the trainer, so the seam is reserved here
//! and filled there. `mailmate-core` never depends on this crate; it sees only the port.

pub mod logreg;

pub use logreg::LogisticRegressionClassifier;

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
