//! The inputs and outputs of a rule evaluation: the resolved field environment, the
//! partitioned result (matched / applied / shadow), the reconstructable explanation, and
//! the conflict-detection output.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::{DecisionId, RuleId, RuleVersionId};
use crate::rules::condition::FieldValue;
use crate::rules::effect::RuleEffect;
use crate::rules::rule::HierarchyBand;

/// The resolved field environment a condition is evaluated against.
///
/// Built at the planning edge from the message, its features, and (for action rules) its
/// classification. The engine only reads this map — it never reaches back to storage to
/// resolve a field, so evaluation is pure and replayable.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RuleEvaluationContext {
    /// The decision this evaluation belongs to.
    pub decision_id: DecisionId,
    /// `field name → value` bindings the predicates resolve against.
    pub fields: BTreeMap<String, FieldValue>,
}

impl RuleEvaluationContext {
    /// Start an empty context for `decision_id`.
    #[must_use]
    pub fn new(decision_id: DecisionId) -> Self {
        Self {
            decision_id,
            fields: BTreeMap::new(),
        }
    }

    /// Bind a field (builder style).
    #[must_use]
    pub fn with(mut self, field: impl Into<String>, value: FieldValue) -> Self {
        self.fields.insert(field.into(), value);
        self
    }
}

/// A rule that matched, with the exact version that decided it — the version is preserved
/// so a decision can be replayed and explained against the content that produced it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MatchedRule {
    /// The rule id.
    pub rule_id: RuleId,
    /// The exact version that matched.
    pub rule_version_id: RuleVersionId,
    /// That version's number.
    pub version_number: i64,
    /// The rule's authority band.
    pub band: HierarchyBand,
    /// Whether the match applied its effect or only recorded it (shadow).
    pub applied: bool,
}

/// An effect an active rule contributed, tagged with its source rule/version/band.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AppliedEffect {
    /// The source rule.
    pub rule_id: RuleId,
    /// The exact version.
    pub rule_version_id: RuleVersionId,
    /// The rule's band (determines precedence; `applied_effects` is sorted by it).
    pub band: HierarchyBand,
    /// The effect contributed.
    pub effect: RuleEffect,
}

/// What a shadow rule *would* have done — recorded, never applied.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ShadowOutcome {
    /// The shadow rule.
    pub rule_id: RuleId,
    /// The exact version.
    pub rule_version_id: RuleVersionId,
    /// The effect it would have applied if it were active.
    pub would_apply: RuleEffect,
}

/// The result of evaluating the rule set against a context.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RuleEvaluationResult {
    /// The decision this result belongs to.
    pub decision_id: DecisionId,
    /// Every rule that matched (active and shadow), ranked by hierarchy band.
    pub matched_rules: Vec<MatchedRule>,
    /// Effects from active rules, winner-first (highest band first).
    pub applied_effects: Vec<AppliedEffect>,
    /// Outcomes recorded for shadow rules (not applied).
    pub shadow_outcomes: Vec<ShadowOutcome>,
}

impl RuleEvaluationResult {
    /// The winning band, if any active rule applied — the band whose effect leads.
    #[must_use]
    pub fn winning_band(&self) -> Option<HierarchyBand> {
        self.applied_effects.first().map(|e| e.band)
    }
}

/// A reconstructable explanation of a decision (`RuleEngine::explain`).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DecisionExplanation {
    /// The decision explained.
    pub decision_id: DecisionId,
    /// The rules that matched, ranked.
    pub matched_rules: Vec<MatchedRule>,
    /// The effects applied, winner-first.
    pub applied_effects: Vec<AppliedEffect>,
    /// The shadow outcomes recorded.
    pub shadow_outcomes: Vec<ShadowOutcome>,
    /// A human-readable narrative.
    pub narrative: String,
}

/// The kind of conflict between two rules.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    /// Same condition, contradictory effects (e.g. move to two different folders).
    ContradictoryEffect,
    /// Overlapping conditions (reserved; overlap reasoning is a later enhancement).
    Overlap,
    /// One rule escalates another into an unsafe action (reserved).
    UnsafeEscalation,
}

impl ConflictKind {
    /// The stable snake_case label stored in `rule_conflicts.conflict_kind`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContradictoryEffect => "contradictory_effect",
            Self::Overlap => "overlap",
            Self::UnsafeEscalation => "unsafe_escalation",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "contradictory_effect" => Some(Self::ContradictoryEffect),
            "overlap" => Some(Self::Overlap),
            "unsafe_escalation" => Some(Self::UnsafeEscalation),
            _ => None,
        }
    }
}

/// How serious a conflict is.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictSeverity {
    /// Low.
    Low,
    /// Medium.
    Medium,
    /// High.
    High,
}

impl ConflictSeverity {
    /// The stable snake_case label stored in `rule_conflicts.severity`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

/// A detected conflict between a candidate rule and an existing rule.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuleConflict {
    /// The existing rule the candidate conflicts with (`None` if the candidate has no id
    /// yet and the conflict is reported against the existing rule only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_rule_id: Option<RuleId>,
    /// What kind of conflict.
    pub kind: ConflictKind,
    /// How serious.
    pub severity: ConflictSeverity,
    /// Human-readable description.
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_builder_binds_fields() {
        let ctx = RuleEvaluationContext::new(DecisionId::from("dec_1"))
            .with("sender_domain", FieldValue::Text("github.com".to_owned()))
            .with("sender_seen_count", FieldValue::Int(9));
        assert_eq!(ctx.fields.len(), 2);
        assert_eq!(
            ctx.fields.get("sender_domain"),
            Some(&FieldValue::Text("github.com".to_owned()))
        );
    }

    #[test]
    fn winning_band_is_the_first_applied_effect() {
        let result = RuleEvaluationResult {
            decision_id: DecisionId::from("dec_1"),
            matched_rules: vec![],
            applied_effects: vec![AppliedEffect {
                rule_id: RuleId::from("rule_a"),
                rule_version_id: RuleVersionId::from("rv_a"),
                band: HierarchyBand::SystemSafety,
                effect: RuleEffect::new(),
            }],
            shadow_outcomes: vec![],
        };
        assert_eq!(result.winning_band(), Some(HierarchyBand::SystemSafety));
    }

    #[test]
    fn conflict_kind_and_severity_labels_round_trip() {
        for kind in [
            ConflictKind::ContradictoryEffect,
            ConflictKind::Overlap,
            ConflictKind::UnsafeEscalation,
        ] {
            assert_eq!(ConflictKind::from_db_str(kind.as_str()), Some(kind));
        }
        assert_eq!(
            ConflictKind::ContradictoryEffect.as_str(),
            "contradictory_effect"
        );
        assert_eq!(ConflictKind::from_db_str("nope"), None);

        for severity in [
            ConflictSeverity::Low,
            ConflictSeverity::Medium,
            ConflictSeverity::High,
        ] {
            assert_eq!(
                ConflictSeverity::from_db_str(severity.as_str()),
                Some(severity)
            );
        }
        assert_eq!(ConflictSeverity::High.as_str(), "high");
        assert_eq!(ConflictSeverity::from_db_str("nope"), None);
    }
}
