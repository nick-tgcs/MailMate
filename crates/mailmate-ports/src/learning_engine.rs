//! The learning-engine port: capture corrections, record cross-cutting provenance,
//! aggregate evidence, and emit reviewable rule proposals.
//!
//! This is the core-facing seam of the learning loop. The default adapter
//! (`mailmate-learning::DefaultLearningEngine`) composes the storage repositories and the
//! deterministic rule engine behind it; the core depends only on this trait. There is no
//! `record_outcome`: rule performance (`RuleOutcome`) is a *view* derived from the
//! feedback tables and `shadow_outcomes`, so it is queried, never written.

use async_trait::async_trait;

use mailmate_common::audit::AuditEntry;
use mailmate_common::error::LearningError;
use mailmate_common::evidence::{EvidenceQuery, RuleEvidence};
use mailmate_common::feedback::TaskFeedback;
use mailmate_common::ids::{AuditId, FeedbackId};
use mailmate_common::proposal::{AgentProposal, ProposalTrigger};

/// Turns user behavior into explicit, reviewable rules.
#[async_trait]
pub trait LearningEngine: Send + Sync {
    /// Capture a task-shaped correction (AI proposal + human correction + reason) into its
    /// per-task feedback table — the single owner of that fact. Returns the new row id.
    ///
    /// # Errors
    /// [`LearningError::Storage`] if the feedback row cannot be persisted.
    async fn record_feedback(&self, feedback: TaskFeedback) -> Result<FeedbackId, LearningError>;

    /// Record a cross-cutting provenance fact with no per-task feedback home (a policy
    /// block, a lifecycle transition, a provider rejection). Returns the new entry id.
    ///
    /// # Errors
    /// [`LearningError::Storage`] if the entry cannot be appended.
    async fn record_audit(&self, entry: AuditEntry) -> Result<AuditId, LearningError>;

    /// Aggregate evidence from the per-task feedback rows matching `query`. The returned
    /// items are *derived* (no `rule_id`/`proposal_id` link yet); a proposal materializes
    /// the link.
    ///
    /// # Errors
    /// [`LearningError::Storage`] if the feedback tables cannot be read.
    async fn collect_evidence(
        &self,
        query: EvidenceQuery,
    ) -> Result<Vec<RuleEvidence>, LearningError>;

    /// Run a proposal pass: cluster evidence, apply the `trigger`'s thresholds, and emit
    /// (and persist) the candidate rule proposals that clear the bar. Each proposal is a
    /// deterministic [`RuleDraft`](mailmate_common::rules::rule::RuleDraft) plus its
    /// supporting evidence — never an activation.
    ///
    /// # Errors
    /// [`LearningError::Storage`] on a persistence failure, or
    /// [`LearningError::InvalidProposal`] if a candidate is not deterministically
    /// expressible.
    async fn propose_candidates(
        &self,
        trigger: ProposalTrigger,
    ) -> Result<Vec<AgentProposal>, LearningError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn LearningEngine) {}
        let _ = takes as fn(&dyn LearningEngine);
    }
}
