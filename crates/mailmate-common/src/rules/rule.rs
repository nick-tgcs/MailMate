//! Rule structure: kind, scope, lifecycle status, risk, the action-hierarchy band, and
//! the immutable version that carries a condition + effect.

use serde::{Deserialize, Serialize};

use crate::actor::Actor;
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

impl RuleKind {
    /// The stable snake_case label stored in a `TEXT` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Classification => "classification",
            Self::Action => "action",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "classification" => Some(Self::Classification),
            "action" => Some(Self::Action),
            _ => None,
        }
    }
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

impl RuleScope {
    /// The stable snake_case label stored in a `TEXT` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Account => "account",
            Self::Folder => "folder",
            Self::Sender => "sender",
            Self::Domain => "domain",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "global" => Some(Self::Global),
            "account" => Some(Self::Account),
            "folder" => Some(Self::Folder),
            "sender" => Some(Self::Sender),
            "domain" => Some(Self::Domain),
            _ => None,
        }
    }
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

    /// The stable snake_case label stored in a `TEXT` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::PendingHumanReview => "pending_human_review",
            Self::ShadowMode => "shadow_mode",
            Self::Active => "active",
            Self::Disabled => "disabled",
            Self::Retired => "retired",
            Self::Rejected => "rejected",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(Self::Draft),
            "pending_human_review" => Some(Self::PendingHumanReview),
            "shadow_mode" => Some(Self::ShadowMode),
            "active" => Some(Self::Active),
            "disabled" => Some(Self::Disabled),
            "retired" => Some(Self::Retired),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
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

impl RiskLevel {
    /// The stable snake_case label stored in a `TEXT` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
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

    /// The stable snake_case label stored in a `TEXT` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SystemSafety => "system_safety",
            Self::HumanHard => "human_hard",
            Self::LearnedActive => "learned_active",
            Self::AgentShadow => "agent_shadow",
            Self::AiSuggestion => "ai_suggestion",
            Self::DefaultFallback => "default_fallback",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "system_safety" => Some(Self::SystemSafety),
            "human_hard" => Some(Self::HumanHard),
            "learned_active" => Some(Self::LearnedActive),
            "agent_shadow" => Some(Self::AgentShadow),
            "ai_suggestion" => Some(Self::AiSuggestion),
            "default_fallback" => Some(Self::DefaultFallback),
            _ => None,
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

/// The immutable content of a rule version, minus the id and monotonic number the
/// repository stamps on persist. Shared by [`NewRule`] (version 1) and [`NewRuleVersion`]
/// (a later revision) so both write the same `*_rule_versions` columns.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RuleVersionContent {
    /// Human title.
    pub title: String,
    /// Explanation.
    pub description: String,
    /// The condition tree.
    pub condition: Condition,
    /// The effect applied (or recorded) when the condition matches.
    pub effect: RuleEffect,
    /// Ordering within the hierarchy band.
    pub priority: i64,
    /// Optional confidence threshold.
    pub confidence_threshold: Option<f64>,
    /// The version's risk level.
    pub risk_level: RiskLevel,
    /// Why this version exists.
    pub change_reason: String,
    /// Who authored it.
    pub created_by: Actor,
}

/// A new rule to persist: identity/scope/band metadata plus the content of version 1. The
/// repository creates the rule row in [`Draft`](RuleStatus::Draft) status and its first
/// immutable version atomically.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct NewRule {
    /// A human-readable, unique-ish name.
    pub stable_name: String,
    /// Which pipeline it belongs to (selects the rule table).
    pub kind: RuleKind,
    /// Its scope.
    pub scope: RuleScope,
    /// Its authority band.
    pub band: HierarchyBand,
    /// Who created it.
    pub created_by: Actor,
    /// The content of version 1.
    pub initial_version: RuleVersionContent,
}

/// A new immutable version appended to an existing rule. The repository assigns the next
/// monotonic `version_number` and re-points the rule's `current_version_id`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct NewRuleVersion {
    /// The rule to append to.
    pub rule_id: RuleId,
    /// Its kind (selects the version table).
    pub kind: RuleKind,
    /// The new version's content.
    pub content: RuleVersionContent,
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

    #[test]
    fn db_string_labels_round_trip_for_every_variant() {
        for kind in [RuleKind::Classification, RuleKind::Action] {
            assert_eq!(RuleKind::from_db_str(kind.as_str()), Some(kind));
        }
        for scope in [
            RuleScope::Global,
            RuleScope::Account,
            RuleScope::Folder,
            RuleScope::Sender,
            RuleScope::Domain,
        ] {
            assert_eq!(RuleScope::from_db_str(scope.as_str()), Some(scope));
        }
        for status in [
            RuleStatus::Draft,
            RuleStatus::PendingHumanReview,
            RuleStatus::ShadowMode,
            RuleStatus::Active,
            RuleStatus::Disabled,
            RuleStatus::Retired,
            RuleStatus::Rejected,
        ] {
            assert_eq!(RuleStatus::from_db_str(status.as_str()), Some(status));
        }
        for risk in [
            RiskLevel::Low,
            RiskLevel::Medium,
            RiskLevel::High,
            RiskLevel::Critical,
        ] {
            assert_eq!(RiskLevel::from_db_str(risk.as_str()), Some(risk));
        }
        for band in [
            HierarchyBand::SystemSafety,
            HierarchyBand::HumanHard,
            HierarchyBand::LearnedActive,
            HierarchyBand::AgentShadow,
            HierarchyBand::AiSuggestion,
            HierarchyBand::DefaultFallback,
        ] {
            assert_eq!(HierarchyBand::from_db_str(band.as_str()), Some(band));
        }
        assert_eq!(RuleStatus::from_db_str("bogus"), None);
        assert_eq!(HierarchyBand::from_db_str("bogus"), None);
    }
}
