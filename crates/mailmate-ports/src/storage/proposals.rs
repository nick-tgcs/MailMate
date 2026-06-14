//! The proposal repository port: persist agent rule-proposals, their supporting evidence
//! links, and drive their review status.
//!
//! A proposal and the `rule_evidence` rows that justify it are written together (the
//! evidence rows carry the `proposal_id` link), so the supporting set is never orphaned.
//! `RuleOutcome` is *not* stored here — it is a derived view (see
//! [`mailmate_common::outcome`]).

use async_trait::async_trait;

use mailmate_common::error::StorageError;
use mailmate_common::evidence::RuleEvidence;
use mailmate_common::ids::ProposalId;
use mailmate_common::proposal::{AgentProposal, ProposalStatus};
use mailmate_common::time::Timestamp;

/// Persistence for agent proposals and their evidence links.
#[async_trait]
pub trait ProposalRepository: Send + Sync {
    /// Persist a proposal together with the evidence rows that justify it, atomically.
    /// Returns the proposal id.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn save(
        &self,
        proposal: AgentProposal,
        evidence: Vec<RuleEvidence>,
    ) -> Result<ProposalId, StorageError>;

    /// Fetch a proposal by id, or `None` if absent.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn get(&self, id: &ProposalId) -> Result<Option<AgentProposal>, StorageError>;

    /// All proposals in `status`, newest first.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn list_by_status(
        &self,
        status: ProposalStatus,
    ) -> Result<Vec<AgentProposal>, StorageError>;

    /// Transition a proposal's review status, recording the review time when supplied.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn set_status(
        &self,
        id: &ProposalId,
        status: ProposalStatus,
        reviewed_at: Option<Timestamp>,
    ) -> Result<(), StorageError>;

    /// The evidence rows linked to a proposal.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn evidence_for(&self, id: &ProposalId) -> Result<Vec<RuleEvidence>, StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn ProposalRepository) {}
        let _ = takes as fn(&dyn ProposalRepository);
    }
}
