//! Rule evidence: the link between an observed event (a per-task feedback row) and the
//! rule or proposal it supports or undermines.
//!
//! Evidence is **derived** from the per-task feedback tables — each feedback row is one
//! observation. The learning engine aggregates these into [`RuleEvidence`] items; when a
//! proposal materializes, the supporting items are persisted into the `rule_evidence`
//! table with the `proposal_id` link (see `agent_proposals`). Evidence is never the
//! source of a fact, only a typed pointer at one — so single-writer-per-fact holds.

use serde::{Deserialize, Serialize};

use crate::ids::{EvidenceId, FeedbackId, MessageId, ProposalId, RuleId};
use crate::rules::rule::RuleKind;
use crate::time::Timestamp;

/// Which per-task feedback table an evidence item was drawn from. Mirrors the table set
/// in *Per-task feedback tables*; the variants not yet captured (their AI function lands
/// in a later phase) are present so the vocabulary is stable across phases.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSourceKind {
    /// `classification_feedback` — junk / phishing / priority / labels.
    Classification,
    /// `filing_feedback` — folder suggestion / move.
    Filing,
    /// `draft_feedback` — reply generation (captured from Phase 8+).
    Draft,
    /// `summary_feedback` — thread summaries (captured from Phase 10+).
    Summary,
    /// `task_extraction_feedback` — extracted tasks (captured from Phase 10+).
    TaskExtraction,
    /// `rule_proposal_feedback` — curator proposal outcomes (captured from Phase 8+).
    RuleProposal,
    /// `followup_feedback` — follow-up cadence/timing (captured from Phase 11+).
    FollowUp,
}

impl EvidenceSourceKind {
    /// The stable snake_case label (also the `source_kind` value in `rule_evidence`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Classification => "classification",
            Self::Filing => "filing",
            Self::Draft => "draft",
            Self::Summary => "summary",
            Self::TaskExtraction => "task_extraction",
            Self::RuleProposal => "rule_proposal",
            Self::FollowUp => "followup",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "classification" => Some(Self::Classification),
            "filing" => Some(Self::Filing),
            "draft" => Some(Self::Draft),
            "summary" => Some(Self::Summary),
            "task_extraction" => Some(Self::TaskExtraction),
            "rule_proposal" => Some(Self::RuleProposal),
            "followup" => Some(Self::FollowUp),
            _ => None,
        }
    }
}

/// What an evidence item argues. Derived from a feedback row's polarity and shape.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// The observation supports the rule/candidate (the AI/rule was right).
    Positive,
    /// The observation undermines it (the AI/rule was wrong) — a correction.
    Negative,
    /// A specific case the candidate must NOT match (a safety counterexample).
    Counterexample,
    /// The user overrode an automatic/suggested action of this shape.
    Override,
}

impl EvidenceKind {
    /// The stable snake_case label stored in `rule_evidence.evidence_kind`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Positive => "positive",
            Self::Negative => "negative",
            Self::Counterexample => "counterexample",
            Self::Override => "override",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "positive" => Some(Self::Positive),
            "negative" => Some(Self::Negative),
            "counterexample" => Some(Self::Counterexample),
            "override" => Some(Self::Override),
            _ => None,
        }
    }
}

/// One evidence item: a typed pointer from a rule/proposal to the feedback row that
/// supports or undermines it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RuleEvidence {
    /// The evidence-row id (`evid_…`).
    pub id: EvidenceId,
    /// The rule kind, when the evidence is attached to a rule (disambiguates `rule_id`).
    pub rule_kind: Option<RuleKind>,
    /// The rule this evidence supports, if any.
    pub rule_id: Option<RuleId>,
    /// The proposal this evidence supports, if any.
    pub proposal_id: Option<ProposalId>,
    /// Which feedback table the supporting row lives in.
    pub source_kind: EvidenceSourceKind,
    /// The id of the supporting per-task feedback row.
    pub source_id: FeedbackId,
    /// The message the observation concerns, if any.
    pub message_id: Option<MessageId>,
    /// What the observation argues.
    pub evidence_kind: EvidenceKind,
    /// Evidence strength (1.0 = one plain observation; weighting is policy, not fact).
    pub weight: f64,
    /// A short, redacted explanation.
    pub summary: String,
    /// When the evidence item was derived/recorded.
    pub created_at: Timestamp,
}

/// A filter over the evidence/feedback space for [`collect_evidence`].
///
/// [`collect_evidence`]: crate placeholder — the `LearningEngine` port method.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct EvidenceQuery {
    /// Restrict to one feedback source (None = every captured source).
    pub source_kind: Option<EvidenceSourceKind>,
    /// Restrict to one message.
    pub message_id: Option<MessageId>,
    /// Cap the number of items returned (None = unbounded).
    pub limit: Option<usize>,
}

impl EvidenceQuery {
    /// An unrestricted query over every captured source.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    /// Restrict the query to a single feedback source.
    #[must_use]
    pub fn for_source(source_kind: EvidenceSourceKind) -> Self {
        Self {
            source_kind: Some(source_kind),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_and_evidence_kind_labels_are_stable() {
        assert_eq!(
            EvidenceSourceKind::Classification.as_str(),
            "classification"
        );
        assert_eq!(EvidenceSourceKind::Filing.as_str(), "filing");
        assert_eq!(
            EvidenceSourceKind::TaskExtraction.as_str(),
            "task_extraction"
        );
        assert_eq!(EvidenceSourceKind::FollowUp.as_str(), "followup");
        assert_eq!(EvidenceKind::Negative.as_str(), "negative");
        assert_eq!(EvidenceKind::Counterexample.as_str(), "counterexample");
    }

    #[test]
    fn source_and_evidence_kind_round_trip_their_db_labels() {
        for source in [
            EvidenceSourceKind::Classification,
            EvidenceSourceKind::Filing,
            EvidenceSourceKind::Draft,
            EvidenceSourceKind::Summary,
            EvidenceSourceKind::TaskExtraction,
            EvidenceSourceKind::RuleProposal,
            EvidenceSourceKind::FollowUp,
        ] {
            assert_eq!(
                EvidenceSourceKind::from_db_str(source.as_str()),
                Some(source)
            );
        }
        for kind in [
            EvidenceKind::Positive,
            EvidenceKind::Negative,
            EvidenceKind::Counterexample,
            EvidenceKind::Override,
        ] {
            assert_eq!(EvidenceKind::from_db_str(kind.as_str()), Some(kind));
        }
        assert_eq!(EvidenceSourceKind::from_db_str("nope"), None);
        assert_eq!(EvidenceKind::from_db_str("nope"), None);
    }

    #[test]
    fn query_builders_set_the_expected_filter() {
        assert_eq!(EvidenceQuery::all(), EvidenceQuery::default());
        let q = EvidenceQuery::for_source(EvidenceSourceKind::Filing);
        assert_eq!(q.source_kind, Some(EvidenceSourceKind::Filing));
        assert_eq!(q.message_id, None);
    }
}
