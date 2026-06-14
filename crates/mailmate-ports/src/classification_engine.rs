//! The classification-engine port: Pipeline 1's "what is this email" verdict.
//!
//! The default adapter (`mailmate-planner::CascadeClassifier`) is the three-tier cascade —
//! deterministic signals + classification rules (Tier 1), the local `Tier2Classifier`
//! (Tier 2), and an LLM `classify_email` escalation (Tier 3) — but the core names only this
//! trait. Determinism-first: a learned classification trait becomes a Tier-1 rule and leaves
//! the model path entirely, so this engine's model use shrinks as rules accrue.

use async_trait::async_trait;

use mailmate_common::classification::{Classification, ClassificationInput};
use mailmate_common::error::ClassificationError;

/// Classifies a message — Pipeline 1 of the two-pipeline flow.
#[async_trait]
pub trait ClassificationEngine: Send + Sync {
    /// Classify `input`, returning the labels, safety scores, priority, and the provenance
    /// of how the verdict was reached.
    ///
    /// # Errors
    /// [`ClassificationError`] if a tier fails (rule evaluation, the local model, or a
    /// Tier-3 provider escalation).
    async fn classify(
        &self,
        input: ClassificationInput,
    ) -> Result<Classification, ClassificationError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn ClassificationEngine) {}
        let _ = takes as fn(&dyn ClassificationEngine);
    }
}
