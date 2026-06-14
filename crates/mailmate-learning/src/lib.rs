//! MailMate learning loop: the adapter that turns user behavior into explicit, reviewable
//! rules.
//!
//! This is an **edge adapter**. It composes the storage repository *ports* and reuses the
//! deterministic rule engine for the crystallization back-test; it never names a backend,
//! and `mailmate-core` never depends on it — the engine is injected at the edge behind the
//! [`LearningEngine`](mailmate_ports::learning_engine::LearningEngine) port.
//!
//! The loop, determinism-first: capture corrections into their single-owner feedback table
//! ([`engine`]) → aggregate evidence and cluster repeated behavior ([`evidence`]) →
//! generate reviewable candidate rules ([`proposals`]) → gate promotion on a historical
//! precision back-test ([`shadow`]) → monitor outcomes as a derived view ([`outcomes`]).
//! Every terminal product is a model-free rule routed through the human-review/shadow gate.

pub mod engine;
pub mod evidence;
pub mod outcomes;
pub mod proposals;
pub mod shadow;

pub use engine::{DefaultLearningEngine, DEFAULT_SOURCE};

/// Returns this crate's package name for smoke tests.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_available() {
        assert_eq!(super::crate_name(), "mailmate-learning");
    }
}
