//! Agent proposals: a reviewable, evidence-backed candidate rule the learning engine (or,
//! from Phase 8, the AI curator) emits. A proposal never activates a rule — it routes
//! through the human-review/shadow gate. It carries the candidate [`RuleDraft`], a
//! redacted rationale, and typed pointers at the feedback rows that justify it.

use serde::{Deserialize, Serialize};

use crate::evidence::EvidenceSourceKind;
use crate::ids::{FeedbackId, ProposalId, RuleId, WorkflowDefId};
use crate::rules::evaluation::RuleConflict;
use crate::rules::rule::{RiskLevel, RuleDraft, RuleKind, RuleStatus};
use crate::time::Timestamp;
use crate::workflow::WorkflowDraft;

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
    /// Propose a brand-new follow-up workflow cadence (Phase 11).
    NewWorkflow,
    /// Refine an existing workflow's cadence offsets.
    RefineWorkflowCadence,
    /// Refine an existing workflow's stop/exit conditions.
    RefineWorkflowStopCondition,
    /// Suggest enrolling a quote/proposal in a workflow.
    SuggestEnrollment,
    /// Retire a stale workflow.
    RetireWorkflow,
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
            Self::NewWorkflow => "new_workflow",
            Self::RefineWorkflowCadence => "refine_workflow_cadence",
            Self::RefineWorkflowStopCondition => "refine_workflow_stop_condition",
            Self::SuggestEnrollment => "suggest_enrollment",
            Self::RetireWorkflow => "retire_workflow",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "new_rule" => Some(Self::NewRule),
            "refine_rule" => Some(Self::RefineRule),
            "merge_rules" => Some(Self::MergeRules),
            "split_rule" => Some(Self::SplitRule),
            "retire_rule" => Some(Self::RetireRule),
            "new_workflow" => Some(Self::NewWorkflow),
            "refine_workflow_cadence" => Some(Self::RefineWorkflowCadence),
            "refine_workflow_stop_condition" => Some(Self::RefineWorkflowStopCondition),
            "suggest_enrollment" => Some(Self::SuggestEnrollment),
            "retire_workflow" => Some(Self::RetireWorkflow),
            _ => None,
        }
    }

    /// Whether this proposal concerns a follow-up workflow rather than a rule.
    #[must_use]
    pub fn is_workflow(self) -> bool {
        matches!(
            self,
            Self::NewWorkflow
                | Self::RefineWorkflowCadence
                | Self::RefineWorkflowStopCondition
                | Self::SuggestEnrollment
                | Self::RetireWorkflow
        )
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

/// The crystallization back-test result captured **at generation time** — the precision and
/// support the promotion gate actually used to admit this candidate — carried on the proposal so
/// the Review card can show "precision 0.88 · support 23 msgs" without re-deriving it (the numbers
/// shown are exactly the numbers the gate judged on).
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct BackTest {
    /// Precision over the domain's whole move history (`correct / fires`). `None` when the
    /// candidate fired on nothing (no division), which the card renders as "—".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precision: Option<f64>,
    /// How many historical messages the candidate reproduced — the back-test support (`fires`).
    pub support: usize,
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
    /// The candidate cadence (present for `new_workflow`; `None` otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_draft: Option<WorkflowDraft>,
    /// An existing target workflow, when the proposal refines/retires/enrolls into one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_workflow_id: Option<WorkflowDefId>,
    /// Typed pointers at the feedback rows that justify the proposal.
    pub evidence_refs: Vec<EvidenceRef>,
    /// The crystallization back-test the gate used to admit this candidate (precision · support),
    /// captured at generation. `None` for proposals with no back-test (classification candidates,
    /// or rows written before this field existed — `#[serde(default)]` keeps them loadable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub back_test: Option<BackTest>,
    /// Conflicts the candidate has with existing active rules — subsumption / co-match overlap /
    /// contradiction — detected at generation. A non-empty list is *why* a proposal is forced to
    /// human review (`recommended_status = pending_human_review`): the human must adjudicate the
    /// overlap before the rule can fire. The card renders these as the "conflicts" measure.
    /// `#[serde(default)]` so older rows (and the many proposals with no conflicts) stay loadable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<RuleConflict>,
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
    /// The crystallization back-test precision bar a filing candidate must clear over the
    /// domain's *whole* move history (agreeing + contradicting) before it is surfaced. A
    /// candidate that also matches mail the user filed elsewhere scores below this and is
    /// withheld — this is what makes [`min_filing_moves`](Self::min_filing_moves) honest rather
    /// than count-only.
    pub filing_precision_bar: f64,
    /// The precision bar an *induced* multi-aspect classification candidate must clear over its
    /// cluster's positives **plus a negative pool** (recent mail the candidate also matches but
    /// on which the user did something else) before it is surfaced. A candidate that mis-fires on
    /// the negative pool scores below this and is withheld — the negative sampling is what makes
    /// a multi-clause rule's precision honest rather than positives-only.
    pub classification_precision_bar: f64,
    /// Minimum times the user has sent mail to a domain before a VIP/priority rule is proposed for
    /// it (learn-from-Sent). People you repeatedly email are people whose inbound mail matters.
    pub min_outbound_sends: usize,
}

impl Default for ProposalThresholds {
    fn default() -> Self {
        Self {
            min_filing_moves: 3,
            min_classification_corrections: 2,
            filing_precision_bar: 0.9,
            classification_precision_bar: 0.9,
            min_outbound_sends: 3,
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
    fn back_test_serializes_compactly_and_is_optional_on_a_proposal() {
        use serde_json::json;
        // A populated back-test serializes as { precision, support }.
        let bt = BackTest {
            precision: Some(0.88),
            support: 23,
        };
        assert_eq!(
            serde_json::to_value(bt).unwrap(),
            json!({ "precision": 0.88, "support": 23 })
        );
        // A fired-on-nothing back-test omits precision (rendered as "—" by the card).
        let empty = BackTest {
            precision: None,
            support: 0,
        };
        assert_eq!(serde_json::to_value(empty).unwrap(), json!({ "support": 0 }));

        // BACKWARD COMPAT: a stored proposal JSON written before `back_test` existed still loads
        // (the field defaults to None) — the #[serde(default)] guarantee.
        let legacy = json!({
            "id": "prop_old",
            "proposal_type": "new_rule",
            "status": "pending_review",
            "title": "t",
            "rationale": "r",
            "risk_level": "low",
            "recommended_status": "shadow_mode",
            "rule_draft": null,
            "target_rule_kind": null,
            "target_rule_id": null,
            "evidence_refs": [],
            "source_provider": "x",
            "created_at": "2026-01-01T00:00:00Z",
            "reviewed_at": null
        });
        let p: AgentProposal = serde_json::from_value(legacy).unwrap();
        assert_eq!(p.back_test, None, "a legacy proposal has no back-test");
    }

    #[test]
    fn default_thresholds_match_the_documented_bar() {
        let t = ProposalThresholds::default();
        assert_eq!(t.min_filing_moves, 3);
        assert_eq!(t.min_classification_corrections, 2);
        assert_eq!(t.filing_precision_bar, 0.9);
        assert_eq!(ProposalTrigger::all().thresholds, t);
        assert_eq!(ProposalTrigger::all().source_kind, None);
    }

    #[test]
    fn workflow_proposal_kinds_round_trip_and_classify() {
        for kind in [
            ProposalKind::NewWorkflow,
            ProposalKind::RefineWorkflowCadence,
            ProposalKind::RefineWorkflowStopCondition,
            ProposalKind::SuggestEnrollment,
            ProposalKind::RetireWorkflow,
        ] {
            assert_eq!(ProposalKind::from_db_str(kind.as_str()), Some(kind));
            assert!(kind.is_workflow());
        }
        for kind in [ProposalKind::NewRule, ProposalKind::RetireRule] {
            assert_eq!(ProposalKind::from_db_str(kind.as_str()), Some(kind));
            assert!(!kind.is_workflow());
        }
        assert_eq!(ProposalKind::from_db_str("nope"), None);
        assert_eq!(ProposalKind::NewWorkflow.as_str(), "new_workflow");
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
