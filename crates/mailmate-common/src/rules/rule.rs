//! Rule structure: kind, scope, lifecycle status, risk, the action-hierarchy band, and
//! the immutable version that carries a condition + effect.

use serde::{Deserialize, Serialize};

use crate::ids::{RuleId, RuleVersionId};
use crate::rules::condition::Condition;
use crate::rules::effect::RuleEffect;

/// Which pipeline a rule belongs to. Classification rules produce labels; action rules
/// produce actions and may reference `classification.*` fields.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    /// A classification (P1) rule.
    Classification,
    /// An action (P2) rule.
    Action,
}

/// A rule's scope — how broadly it applies.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleScope {
    /// Applies everywhere.
    Global,
    /// Applies within an account.
    Account,
    /// Applies within a folder.
    Folder,
    /// Applies to a sender.
    Sender,
    /// Applies to a domain.
    Domain,
}

/// A rule's lifecycle status. Only [`Active`](RuleStatus::Active) and
/// [`ShadowMode`](RuleStatus::ShadowMode) are evaluated; everything else never fires.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleStatus {
    /// Early candidate, not ready for review.
    Draft,
    /// Awaiting human approval.
    PendingHumanReview,
    /// Evaluated and logged, but actions are not applied.
    ShadowMode,
    /// Live: can affect action planning (subject to the policy guard).
    Active,
    /// Stored but never fires.
    Disabled,
    /// Obsolete, preserved for audit.
    Retired,
    /// Rejected by a human; not to be reproposed without new evidence.
    Rejected,
}

impl RuleStatus {
    /// Whether a rule in this status is evaluated at all (active rules apply; shadow rules
    /// only record). Draft/pending/disabled/retired/rejected rules never fire.
    #[must_use]
    pub fn is_evaluated(self) -> bool {
        matches!(self, Self::Active | Self::ShadowMode)
    }

    /// Whether a matching rule in this status applies its effect (active only).
    #[must_use]
    pub fn applies_effect(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// A rule's risk level, governing whether it should pass through shadow mode first.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// Low risk.
    Low,
    /// Medium risk.
    Medium,
    /// High risk.
    High,
    /// Critical risk.
    Critical,
}

/// The action-hierarchy band a rule occupies — its authority class, independent of its
/// lifecycle status. Lower [`ordinal`](HierarchyBand::ordinal) outranks higher.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HierarchyBand {
    /// Built-in safety invariants — outrank everything.
    SystemSafety,
    /// Explicit human hard rules.
    HumanHard,
    /// Human-approved learned rules.
    LearnedActive,
    /// Agent-proposed draft/shadow rules — evidence only, no risky actions.
    AgentShadow,
    /// AI provider suggestions — advisory only.
    AiSuggestion,
    /// Conservative default when nothing else applies.
    DefaultFallback,
}

impl HierarchyBand {
    /// The precedence ordinal (0 = highest authority).
    #[must_use]
    pub fn ordinal(self) -> u8 {
        match self {
            Self::SystemSafety => 0,
            Self::HumanHard => 1,
            Self::LearnedActive => 2,
            Self::AgentShadow => 3,
            Self::AiSuggestion => 4,
            Self::DefaultFallback => 5,
        }
    }
}

/// An immutable rule version: the condition→effect content at a point in time.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RuleVersion {
    /// The version id (`rv_…`).
    pub id: RuleVersionId,
    /// Monotonic version number within the rule.
    pub version_number: i64,
    /// The condition tree.
    pub condition: Condition,
    /// The effect applied (or recorded) when the condition matches.
    pub effect: RuleEffect,
    /// The version's risk level.
    pub risk_level: RiskLevel,
}

/// A rule ready for evaluation: its identity, band, status, and current version. This is
/// the engine-facing projection of the stored rule + its current version.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EvaluatableRule {
    /// The rule id (`rule_…`).
    pub rule_id: RuleId,
    /// Which pipeline it belongs to.
    pub kind: RuleKind,
    /// Its scope.
    pub scope: RuleScope,
    /// Its authority band.
    pub band: HierarchyBand,
    /// Its lifecycle status.
    pub status: RuleStatus,
    /// The current version's content.
    pub version: RuleVersion,
}

/// A candidate rule submitted for conflict detection (no id/version yet).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RuleDraft {
    /// Which pipeline it would belong to.
    pub kind: RuleKind,
    /// Its proposed scope.
    pub scope: RuleScope,
    /// Its proposed condition.
    pub condition: Condition,
    /// Its proposed effect.
    pub effect: RuleEffect,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_active_and_shadow_are_evaluated() {
        assert!(RuleStatus::Active.is_evaluated());
        assert!(RuleStatus::ShadowMode.is_evaluated());
        for status in [
            RuleStatus::Draft,
            RuleStatus::PendingHumanReview,
            RuleStatus::Disabled,
            RuleStatus::Retired,
            RuleStatus::Rejected,
        ] {
            assert!(!status.is_evaluated(), "{status:?} must not fire");
        }
        assert!(RuleStatus::Active.applies_effect());
        assert!(!RuleStatus::ShadowMode.applies_effect());
    }

    #[test]
    fn hierarchy_band_ordinals_are_strictly_increasing() {
        let bands = [
            HierarchyBand::SystemSafety,
            HierarchyBand::HumanHard,
            HierarchyBand::LearnedActive,
            HierarchyBand::AgentShadow,
            HierarchyBand::AiSuggestion,
            HierarchyBand::DefaultFallback,
        ];
        for pair in bands.windows(2) {
            assert!(pair[0].ordinal() < pair[1].ordinal());
            assert!(pair[0] < pair[1], "derived Ord must match ordinal");
        }
    }

    #[test]
    fn risk_levels_order_low_to_critical() {
        assert!(RiskLevel::Low < RiskLevel::Medium);
        assert!(RiskLevel::High < RiskLevel::Critical);
    }
}
