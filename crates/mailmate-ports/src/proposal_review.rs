//! The proposal-review port: apply a human's decision to an agent proposal.
//!
//! This is where "require human review for activation of risky rules" is enforced as a
//! capability: a proposal's recommended rule is created **only** through
//! [`review`](ProposalReview::review) with an accepting decision, and *materialization* always
//! enters the recommended status (`shadow_mode`/`pending_human_review`), never `active`. Going
//! live is a separate, explicit step: an accepting decision may also carry
//! [`ReviewDecision::activate`], which performs a distinct `rule_activated` transition to
//! `active` — so no rule is ever activated as a silent side effect of materialization. The
//! decision is recorded to `rule_proposal_feedback` (so the curator loop is learnable) and to
//! the audit timeline.
//!
//! The default adapter (`mailmate-learning::DefaultProposalReview`) composes the proposal,
//! rule, feedback, and audit storage ports; the core names only this trait.

use async_trait::async_trait;

use mailmate_common::curator::{ReviewDecision, ReviewOutcome};
use mailmate_common::error::ReviewError;
use mailmate_common::proposal::AgentProposal;

/// Applies human accept/reject decisions to agent proposals.
#[async_trait]
pub trait ProposalReview: Send + Sync {
    /// The proposals awaiting a human decision, newest first.
    ///
    /// # Errors
    /// [`ReviewError::Storage`] on a backend failure.
    async fn pending(&self) -> Result<Vec<AgentProposal>, ReviewError>;

    /// Apply `decision`: on acceptance, create the recommended rule (in its recommended,
    /// non-active status) and record an acceptance; on rejection, record a rejection. Either
    /// way, transition the proposal and write the `rule_proposal_feedback` row.
    ///
    /// # Errors
    /// [`ReviewError::NotFound`] if the proposal is unknown; [`ReviewError::AlreadyReviewed`]
    /// if it is in a terminal status; [`ReviewError::MissingDraft`] if an acceptance asks to
    /// create a rule from a proposal that carries none; [`ReviewError::Storage`] on a backend
    /// failure.
    async fn review(&self, decision: ReviewDecision) -> Result<ReviewOutcome, ReviewError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn ProposalReview) {}
        let _ = takes as fn(&dyn ProposalReview);
    }
}
