//! The agent-curator port: the AI advisor over the explicit rule system.
//!
//! The curator is the **teacher**, not the executor (determinism-first): it consumes a
//! [`CuratorRequest`], consults the AI provider plus the deterministic rule/evidence state,
//! and returns a [`CuratorReport`] of reviewable proposals and advisory observations. It
//! never activates a rule — every proposal it emits is `pending_review` and recommends
//! shadow/human-review, and activation is a separate decision behind
//! [`ProposalReview`](crate::proposal_review::ProposalReview).
//!
//! The default adapter (`mailmate-learning::AiRuleCurator`) composes the `AiProvider`,
//! `LearningEngine`, and the rule/proposal/conflict storage ports; the core names only this
//! trait, so no provider- or backend-specific detail crosses the boundary.

use async_trait::async_trait;

use mailmate_common::curator::{CuratorReport, CuratorRequest};
use mailmate_common::error::CuratorError;

/// Improves the explicit rule system by proposing (never activating) changes.
#[async_trait]
pub trait RuleCurator: Send + Sync {
    /// Run a curator pass: consult the provider and the deterministic state for the
    /// requested operations, persist any proposals (as `pending_review`) and any
    /// rule-vs-rule conflicts found, and return everything the pass produced.
    ///
    /// # Errors
    /// [`CuratorError::Provider`] if the provider fails or its response does not validate
    /// (the failure is audited and drives no proposal); [`CuratorError::Storage`] or
    /// [`CuratorError::Rules`] on a backend/rule-engine failure.
    async fn curate(&self, request: CuratorRequest) -> Result<CuratorReport, CuratorError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn RuleCurator) {}
        let _ = takes as fn(&dyn RuleCurator);
    }
}
