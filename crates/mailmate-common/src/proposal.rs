//! Agent proposals: a reviewable, evidence-backed candidate rule the learning engine (or,
//! from Phase 8, the AI curator) emits. A proposal never activates a rule — it routes
//! through the human-review/shadow gate. It carries the candidate [`RuleDraft`], a
//! redacted rationale, and typed pointers at the feedback rows that justify it.

use serde::{Deserialize, Serialize};

use crate::evidence::EvidenceSourceKind;
use crate::ids::{FeedbackId, ProposalId, RuleId};
use crate::rules::rule::{RiskLevel, RuleDraft, RuleKind, RuleStatus};
use crate::time::Timestamp;

/// What a proposal asks for. Phase 7's learning engine only emits [`NewRule`]; the richer
/// kinds (`refine`, `merge`, `split`, `retire`) land with the AI curator in Phase 8, but
/// the vocabulary is stable from here.
///
/// [`NewRule`]: ProposalKind::NewRule
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalKind {
    /// Create a brand-new rule.
    NewRule,
    /// Refine an existing rule's condition/effect.
    RefineRule,
    /// Merge two overlapping rules.
    MergeRules,
    /// Split one broad rule into narrower ones.
    SplitRule,
    /// Retire a stale rule.
    RetireRule,
}

impl ProposalKind {
    /// The stable snake_case label stored in `agent_proposals.proposal_type`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NewRule => "new_rule",
            Self::RefineRule => "refine_rule",
            Self::MergeRules => "merge_rules",
            Self::SplitRule => "split_rule",
            Self::RetireRule => "retire_rule",
        }
    }
}

/// A proposal's own review status (distinct from a rule's [`RuleStatus`]). This tracks
/// the *proposal's* lifecycle from emission to disposition; the rule it recommends carries
/// its own [`RuleStatus`] once created.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    /// Emitted but not yet surfaced for review.
    Draft,
    /// Awaiting a human decision.
    PendingReview,
    /// Accepted — the recommended rule was created (possibly with edits).
    Accepted,
    /// Rejected by a human; not to be re-proposed without new evidence.
    Rejected,
    /// Being shadow-tested before activation.
    Shadowing,
}

impl ProposalStatus {
    /// The stable snake_case label stored in `agent_proposals.status`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::PendingReview => "pending_review",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Shadowing => "shadowing",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(Self::Draft),
            "pending_review" => Some(Self::PendingReview),
            "accepted" => Some(Self::Accepted),
            "rejected" => Some(Self::Rejected),
            "shadowing" => Some(Self::Shadowing),
            _ => None,
        }
    }

    /// Whether this is a terminal disposition (no further transitions).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Accepted | Self::Rejected)
    }
}

/// A typed pointer at one supporting feedback row (`{ kind, id }` in the proposal JSON).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceRef {
    /// Which feedback table the row lives in.
    pub kind: EvidenceSourceKind,
    /// The supporting feedback row id.
    pub id: FeedbackId,
}

/// An AI-/learning-generated proposal requiring human review or shadow testing.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AgentProposal {
    /// The proposal id (`prop_…`).
    pub id: ProposalId,
    /// What the proposal asks for.
    pub proposal_type: ProposalKind,
    /// The proposal's review status.
    pub status: ProposalStatus,
    /// A short human title.
    pub title: String,
    /// A redacted, human-readable justification.
    pub rationale: String,
    /// The estimated risk of the recommended rule.
    pub risk_level: RiskLevel,
    /// The lifecycle status the proposal recommends the rule enter on acceptance
    /// (`pending_human_review` for risky rules, `shadow_mode` for cautious activation).
    pub recommended_status: RuleStatus,
    /// The candidate rule (present for `new_rule`; `None` for kinds that reference an
    /// existing rule by [`target_rule_id`](AgentProposal::target_rule_id)).
    pub rule_draft: Option<RuleDraft>,
    /// The kind of an existing target rule, when the proposal refines/splits/retires one.
    pub target_rule_kind: Option<RuleKind>,
    /// An existing target rule, when the proposal refines/splits/retires one.
    pub target_rule_id: Option<RuleId>,
    /// Typed pointers at the feedback rows that justify the proposal.
    pub evidence_refs: Vec<EvidenceRef>,
    /// The provider/learning-engine source label.
    pub source_provider: String,
    /// When emitted.
    pub created_at: Timestamp,
    /// When a human reviewed it, if reviewed.
    pub reviewed_at: Option<Timestamp>,
}

/// The thresholds that gate a candidate from "observed pattern" to "proposed rule" — the
/// learning loop's evidence bar (see *Proposal thresholds*). Defaults mirror the doc:
/// 3 similar manual moves from the same domain to the same folder; 2 high-confidence
/// classification corrections of the same label from the same domain.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProposalThresholds {
    /// Minimum same-domain → same-folder moves before a filing rule is proposed.
    pub min_filing_moves: usize,
    /// Minimum same-label, same-domain corrections before a classification rule is proposed.
    pub min_classification_corrections: usize,
}

impl Default for ProposalThresholds {
    fn default() -> Self {
        Self {
            min_filing_moves: 3,
            min_classification_corrections: 2,
        }
    }
}

/// What kicks a proposal pass. Carries the evidence bar and an optional source filter.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ProposalTrigger {
    /// Restrict the pass to one feedback source (None = every captured source).
    pub source_kind: Option<EvidenceSourceKind>,
    /// The evidence bar to clear.
    pub thresholds: ProposalThresholds,
}

impl ProposalTrigger {
    /// A pass over every captured source at the default evidence bar.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_and_status_labels_are_stable() {
        assert_eq!(ProposalKind::NewRule.as_str(), "new_rule");
        assert_eq!(ProposalKind::SplitRule.as_str(), "split_rule");
        assert_eq!(ProposalStatus::PendingReview.as_str(), "pending_review");
        assert_eq!(ProposalStatus::Shadowing.as_str(), "shadowing");
        assert!(ProposalStatus::Accepted.is_terminal());
        assert!(ProposalStatus::Rejected.is_terminal());
        assert!(!ProposalStatus::PendingReview.is_terminal());
        for status in [
            ProposalStatus::Draft,
            ProposalStatus::PendingReview,
            ProposalStatus::Accepted,
            ProposalStatus::Rejected,
            ProposalStatus::Shadowing,
        ] {
            assert_eq!(ProposalStatus::from_db_str(status.as_str()), Some(status));
        }
        assert_eq!(ProposalStatus::from_db_str("nope"), None);
    }

    #[test]
    fn default_thresholds_match_the_documented_bar() {
        let t = ProposalThresholds::default();
        assert_eq!(t.min_filing_moves, 3);
        assert_eq!(t.min_classification_corrections, 2);
        assert_eq!(ProposalTrigger::all().thresholds, t);
        assert_eq!(ProposalTrigger::all().source_kind, None);
    }

    #[test]
    fn evidence_ref_serializes_as_kind_and_id() {
        let r = EvidenceRef {
            kind: EvidenceSourceKind::Filing,
            id: FeedbackId::from("filfb_001"),
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["kind"], "filing");
        assert_eq!(json["id"], "filfb_001");
    }
}
