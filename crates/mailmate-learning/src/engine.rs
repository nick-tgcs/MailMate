//! [`DefaultLearningEngine`]: the [`LearningEngine`] port over the storage repositories.
//!
//! It captures corrections into their single-owner feedback table, records cross-cutting
//! provenance into the audit timeline, derives evidence from the feedback rows, and runs a
//! proposal pass that clusters repeated behavior into reviewable candidate rules. It never
//! activates a rule — every proposal it emits is `pending_review` and recommends
//! `shadow_mode`; the human-review and back-test gates stand between a proposal and a live
//! rule.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;

use mailmate_common::actor::Actor;
use mailmate_common::audit::{event_type, AuditEntry};
use mailmate_common::error::LearningError;
use mailmate_common::evidence::{EvidenceQuery, EvidenceSourceKind, RuleEvidence};
use mailmate_common::feedback::{
    ClassificationFeedback, ClassificationFeedbackQuery, FilingFeedback, FilingFeedbackQuery,
    TaskFeedback,
};
use mailmate_common::ids::{AuditId, FeedbackId};
use mailmate_common::proposal::{AgentProposal, ProposalStatus, ProposalTrigger};
use mailmate_ports::learning_engine::LearningEngine;
use mailmate_ports::storage::audit::AuditRepository;
use mailmate_ports::storage::feedback::FeedbackRepository;
use mailmate_ports::storage::proposals::ProposalRepository;

use crate::evidence::{
    cluster_classification, cluster_filing, evidence_from_classification, evidence_from_filing,
};
use crate::proposals::{classification_proposal, filing_proposal};

/// The default source label stamped on proposals and audit entries this engine emits.
pub const DEFAULT_SOURCE: &str = "learning-engine";

/// The default learning engine, composing the per-task feedback repositories, the audit
/// timeline, and the proposal store.
pub struct DefaultLearningEngine {
    classification_feedback: Arc<dyn FeedbackRepository<ClassificationFeedback>>,
    filing_feedback: Arc<dyn FeedbackRepository<FilingFeedback>>,
    audit: Arc<dyn AuditRepository>,
    proposals: Arc<dyn ProposalRepository>,
    source_label: String,
}

impl DefaultLearningEngine {
    /// Build an engine over the given repositories, using [`DEFAULT_SOURCE`] as the source
    /// label.
    #[must_use]
    pub fn new(
        classification_feedback: Arc<dyn FeedbackRepository<ClassificationFeedback>>,
        filing_feedback: Arc<dyn FeedbackRepository<FilingFeedback>>,
        audit: Arc<dyn AuditRepository>,
        proposals: Arc<dyn ProposalRepository>,
    ) -> Self {
        Self {
            classification_feedback,
            filing_feedback,
            audit,
            proposals,
            source_label: DEFAULT_SOURCE.to_owned(),
        }
    }

    /// Override the source label stamped on emitted proposals/audit entries.
    #[must_use]
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source_label = source.into();
        self
    }

    /// Persist a proposal with its evidence and record the proposal in the audit timeline.
    async fn persist_proposal(
        &self,
        proposal: &AgentProposal,
        evidence: Vec<RuleEvidence>,
    ) -> Result<(), LearningError> {
        self.proposals.save(proposal.clone(), evidence).await?;
        let entry = AuditEntry::new(event_type::RULE_PROPOSED, Actor::Ai)
            .with_proposal(proposal.id.clone())
            .with_payload(serde_json::json!({
                "title": proposal.title,
                "proposal_type": proposal.proposal_type.as_str(),
                "recommended_status": proposal.recommended_status.as_str(),
            }));
        self.audit.append(entry).await?;
        Ok(())
    }

    /// The signatures of every proposal already on record — the dedup set a fresh pass checks
    /// against. It spans the pending/shadowing states and the terminal accepted/rejected
    /// dispositions, so a cluster that has already been proposed (whatever its fate) is not
    /// proposed again — in particular, a rejected cluster is not re-proposed without new
    /// evidence (the invariant on [`ProposalStatus::Rejected`]).
    async fn existing_signatures(&self) -> Result<HashSet<String>, LearningError> {
        let mut signatures = HashSet::new();
        for status in [
            ProposalStatus::Draft,
            ProposalStatus::PendingReview,
            ProposalStatus::Shadowing,
            ProposalStatus::Accepted,
            ProposalStatus::Rejected,
        ] {
            for proposal in self.proposals.list_by_status(status).await? {
                signatures.insert(proposal_signature(&proposal));
            }
        }
        Ok(signatures)
    }
}

/// A deterministic identity for *what a proposal would change* — its kind plus the canonical
/// JSON of its candidate rule/workflow draft. Two passes over the same recurring pattern build
/// byte-identical drafts, hence the same signature; this is the cross-pass dedup key that makes
/// [`DefaultLearningEngine::propose_candidates`] idempotent (re-running adds nothing new).
fn proposal_signature(proposal: &AgentProposal) -> String {
    let rule = serde_json::to_string(&proposal.rule_draft).unwrap_or_default();
    let workflow = serde_json::to_string(&proposal.workflow_draft).unwrap_or_default();
    format!("{}|{rule}|{workflow}", proposal.proposal_type.as_str())
}

fn wants(filter: Option<EvidenceSourceKind>, kind: EvidenceSourceKind) -> bool {
    filter.is_none() || filter == Some(kind)
}

#[async_trait]
impl LearningEngine for DefaultLearningEngine {
    async fn record_feedback(&self, feedback: TaskFeedback) -> Result<FeedbackId, LearningError> {
        let id = match feedback {
            TaskFeedback::Classification(row) => self.classification_feedback.append(row).await?,
            TaskFeedback::Filing(row) => self.filing_feedback.append(row).await?,
        };
        Ok(id)
    }

    async fn record_audit(&self, entry: AuditEntry) -> Result<AuditId, LearningError> {
        Ok(self.audit.append(entry).await?)
    }

    async fn collect_evidence(
        &self,
        query: EvidenceQuery,
    ) -> Result<Vec<RuleEvidence>, LearningError> {
        let mut out = Vec::new();
        if wants(query.source_kind, EvidenceSourceKind::Classification) {
            let rows = self
                .classification_feedback
                .query(ClassificationFeedbackQuery {
                    message_id: query.message_id.clone(),
                    human_label: None,
                    limit: query.limit,
                })
                .await?;
            out.extend(rows.iter().map(evidence_from_classification));
        }
        if wants(query.source_kind, EvidenceSourceKind::Filing) {
            let rows = self
                .filing_feedback
                .query(FilingFeedbackQuery {
                    message_id: query.message_id.clone(),
                    human_chosen_folder: None,
                    limit: query.limit,
                })
                .await?;
            out.extend(rows.iter().map(evidence_from_filing));
        }
        if let Some(limit) = query.limit {
            out.truncate(limit);
        }
        Ok(out)
    }

    async fn propose_candidates(
        &self,
        trigger: ProposalTrigger,
    ) -> Result<Vec<AgentProposal>, LearningError> {
        let mut proposals = Vec::new();
        // Idempotency: a recurring cluster proposes ONCE. We dedup each candidate by its
        // deterministic signature against everything already proposed, so re-running a pass
        // (on every tick / launch) adds nothing new and never piles up duplicates. `seen` is
        // seeded from the store and then also collects this pass's emissions.
        let mut seen = self.existing_signatures().await?;

        if wants(trigger.source_kind, EvidenceSourceKind::Filing) {
            let rows = self
                .filing_feedback
                .query(FilingFeedbackQuery::default())
                .await?;
            for cluster in cluster_filing(rows) {
                if cluster.rows.len() >= trigger.thresholds.min_filing_moves {
                    let (proposal, evidence) = filing_proposal(&cluster, &self.source_label);
                    if seen.insert(proposal_signature(&proposal)) {
                        self.persist_proposal(&proposal, evidence).await?;
                        proposals.push(proposal);
                    }
                }
            }
        }

        if wants(trigger.source_kind, EvidenceSourceKind::Classification) {
            let rows = self
                .classification_feedback
                .query(ClassificationFeedbackQuery::default())
                .await?;
            for cluster in cluster_classification(rows) {
                if cluster.rows.len() >= trigger.thresholds.min_classification_corrections {
                    let (proposal, evidence) =
                        classification_proposal(&cluster, &self.source_label);
                    if seen.insert(proposal_signature(&proposal)) {
                        self.persist_proposal(&proposal, evidence).await?;
                        proposals.push(proposal);
                    }
                }
            }
        }

        Ok(proposals)
    }
}
