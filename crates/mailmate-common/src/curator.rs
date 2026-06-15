//! The agent-curator vocabulary: what a curator pass is *asked* to do, what it *returns*,
//! and how a human *disposes* of the proposals it surfaces.
//!
//! The curator is the AI advisor over the explicit rule system. It may **propose** — new
//! rules, refinements, merges, splits, retirements — and it may **observe** — conflicts
//! between live rules, threshold suggestions, feedback summaries. It may never *activate*:
//! every proposal it emits is `pending_review` and recommends `shadow_mode` (or, for risky
//! rules, `pending_human_review`). Activation is a separate human decision, recorded via a
//! [`ReviewDecision`].

use serde::{Deserialize, Serialize};

use crate::conflict::RuleConflictRecord;
use crate::evidence::EvidenceSourceKind;
use crate::ids::{FeedbackId, ProposalId, RuleId};
use crate::proposal::{AgentProposal, ProposalStatus};
use crate::rules::rule::RuleKind;

/// One thing a curator pass can do. A request carries a set of these (empty = all); the
/// adapter runs each requested capability and folds the results into one [`CuratorReport`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CuratorOperation {
    /// Propose brand-new rules from observed behavior.
    Propose,
    /// Refine an existing rule's condition/effect.
    Refine,
    /// Merge duplicate/overlapping rules.
    Merge,
    /// Split an over-broad rule.
    Split,
    /// Flag rules that no longer earn their keep.
    DetectStale,
    /// Scan live rules for contradictory pairs (records `rule_conflicts`).
    DetectConflicts,
    /// Suggest changes to the evidence thresholds.
    SuggestThresholds,
    /// Summarize the recent user-feedback patterns.
    SummarizeFeedback,
}

impl CuratorOperation {
    /// Every capability, in a stable order.
    #[must_use]
    pub fn all() -> [CuratorOperation; 8] {
        [
            Self::Propose,
            Self::Refine,
            Self::Merge,
            Self::Split,
            Self::DetectStale,
            Self::DetectConflicts,
            Self::SuggestThresholds,
            Self::SummarizeFeedback,
        ]
    }

    /// The stable snake_case label (also the operation list sent to the provider).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Propose => "propose",
            Self::Refine => "refine",
            Self::Merge => "merge",
            Self::Split => "split",
            Self::DetectStale => "detect_stale",
            Self::DetectConflicts => "detect_conflicts",
            Self::SuggestThresholds => "suggest_thresholds",
            Self::SummarizeFeedback => "summarize_feedback",
        }
    }

    /// Whether this operation asks the provider to emit rule proposals (as opposed to the
    /// deterministic, model-free observations).
    #[must_use]
    pub fn is_proposing(self) -> bool {
        matches!(
            self,
            Self::Propose | Self::Refine | Self::Merge | Self::Split | Self::DetectStale
        )
    }
}

/// What a curator pass is asked to do, and over which rule kind.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct CuratorRequest {
    /// The capabilities to run. Empty means *every* capability.
    pub operations: Vec<CuratorOperation>,
    /// Restrict the pass to one rule kind (None = both pipelines).
    pub rule_kind: Option<RuleKind>,
}

impl CuratorRequest {
    /// A full pass: every capability, both rule kinds.
    #[must_use]
    pub fn full() -> Self {
        Self::default()
    }

    /// A pass running just one capability.
    #[must_use]
    pub fn just(operation: CuratorOperation) -> Self {
        Self {
            operations: vec![operation],
            rule_kind: None,
        }
    }

    /// Whether `operation` is in scope for this request (an empty set means *all*).
    #[must_use]
    pub fn wants(&self, operation: CuratorOperation) -> bool {
        self.operations.is_empty() || self.operations.contains(&operation)
    }
}

/// An advisory suggestion to change an evidence threshold. The curator may *suggest*; the
/// thresholds are never auto-changed.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ThresholdSuggestion {
    /// Which rule kind the threshold gates.
    pub rule_kind: RuleKind,
    /// Which feedback source the threshold counts.
    pub source_kind: EvidenceSourceKind,
    /// The value the curator suggests.
    pub suggested: usize,
    /// Why.
    pub rationale: String,
}

/// An advisory, redacted summary of a feedback source's recent pattern.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FeedbackSummary {
    /// Which feedback source this summarizes.
    pub source_kind: EvidenceSourceKind,
    /// How many rows informed the summary.
    pub sample_size: usize,
    /// The summary text.
    pub text: String,
}

/// Everything one curator pass produced: the persisted proposals, the recorded conflicts,
/// and the advisory observations.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct CuratorReport {
    /// Proposals emitted and persisted (each `pending_review`, recommending shadow/review).
    pub proposals: Vec<AgentProposal>,
    /// Conflicts found between live rules and recorded in `rule_conflicts`.
    pub conflicts: Vec<RuleConflictRecord>,
    /// Advisory threshold suggestions (never auto-applied).
    pub threshold_suggestions: Vec<ThresholdSuggestion>,
    /// Advisory feedback summaries.
    pub feedback_summaries: Vec<FeedbackSummary>,
}

impl CuratorReport {
    /// Whether the pass produced nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.proposals.is_empty()
            && self.conflicts.is_empty()
            && self.threshold_suggestions.is_empty()
            && self.feedback_summaries.is_empty()
    }
}

/// A human's disposition of one proposal. Acceptance is the **only** path by which a
/// proposal's recommended rule is created — and even then it enters its recommended status
/// (`shadow_mode`/`pending_human_review`), never `active` directly.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ReviewDecision {
    /// The proposal being decided.
    pub proposal_id: ProposalId,
    /// What the human decided.
    pub outcome: crate::feedback::ProposalOutcome,
    /// Why — a chip code, if given.
    pub human_reason_code: Option<String>,
    /// Freeform fallback reason.
    pub human_reason_text: Option<String>,
}

impl ReviewDecision {
    /// Accept a proposal as-is.
    #[must_use]
    pub fn accept(proposal_id: ProposalId) -> Self {
        Self {
            proposal_id,
            outcome: crate::feedback::ProposalOutcome::Accepted,
            human_reason_code: None,
            human_reason_text: None,
        }
    }

    /// Reject a proposal, with a reason chip.
    #[must_use]
    pub fn reject(proposal_id: ProposalId, reason_code: impl Into<String>) -> Self {
        Self {
            proposal_id,
            outcome: crate::feedback::ProposalOutcome::Rejected,
            human_reason_code: Some(reason_code.into()),
            human_reason_text: None,
        }
    }
}

/// What a review produced: the proposal's new status, the rule it created (if any), and the
/// `rule_proposal_feedback` row that recorded the decision.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ReviewOutcome {
    /// The reviewed proposal.
    pub proposal_id: ProposalId,
    /// The proposal's status after review (`accepted`/`rejected`).
    pub new_status: ProposalStatus,
    /// The rule created on acceptance (a `new_rule` proposal), if one was.
    pub created_rule_id: Option<RuleId>,
    /// The `rule_proposal_feedback` row that recorded the decision.
    pub feedback_id: FeedbackId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_labels_are_stable_and_all_is_complete() {
        assert_eq!(CuratorOperation::all().len(), 8);
        assert_eq!(CuratorOperation::Propose.as_str(), "propose");
        assert_eq!(
            CuratorOperation::DetectConflicts.as_str(),
            "detect_conflicts"
        );
        assert!(CuratorOperation::Refine.is_proposing());
        assert!(!CuratorOperation::DetectConflicts.is_proposing());
        assert!(!CuratorOperation::SummarizeFeedback.is_proposing());
    }

    #[test]
    fn full_request_wants_everything_and_focused_wants_only_one() {
        let full = CuratorRequest::full();
        assert!(full.operations.is_empty());
        assert!(full.wants(CuratorOperation::Propose));
        assert!(full.wants(CuratorOperation::DetectConflicts));

        let focused = CuratorRequest::just(CuratorOperation::DetectConflicts);
        assert!(focused.wants(CuratorOperation::DetectConflicts));
        assert!(!focused.wants(CuratorOperation::Propose));
    }

    #[test]
    fn empty_report_is_empty() {
        assert!(CuratorReport::default().is_empty());
    }

    #[test]
    fn review_decision_helpers_set_outcome_and_polarity() {
        let accept = ReviewDecision::accept(ProposalId::from("prop_1"));
        assert_eq!(accept.outcome, crate::feedback::ProposalOutcome::Accepted);
        let reject = ReviewDecision::reject(ProposalId::from("prop_2"), "too_broad");
        assert_eq!(reject.outcome, crate::feedback::ProposalOutcome::Rejected);
        assert_eq!(reject.human_reason_code.as_deref(), Some("too_broad"));
    }
}
