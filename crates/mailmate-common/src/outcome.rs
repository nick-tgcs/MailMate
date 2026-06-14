//! Rule performance as a **derived value**, never a stored fact.
//!
//! `RuleOutcome` is the *view* the architecture insists rule performance must be: it is
//! computed by aggregating the per-task feedback rows that name a rule (`matched_rule_id`)
//! and the `shadow_outcomes` rows a shadow rule produced. Because it is derived, it cannot
//! drift from or duplicate its sources. The aggregation itself lives in the learning
//! adapter (`mailmate-learning::outcomes`); this is the shared shape it returns.

use serde::{Deserialize, Serialize};

use crate::ids::RuleId;
use crate::rules::rule::RuleKind;

/// Aggregated performance for one rule, derived from feedback + shadow outcomes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RuleOutcome {
    /// The rule this performance concerns.
    pub rule_id: RuleId,
    /// Which pipeline it belongs to.
    pub rule_kind: RuleKind,
    /// How many captured feedback rows named this rule as the one that fired.
    pub fire_count: u64,
    /// Of those, how many the human agreed with (polarity positive).
    pub positive_count: u64,
    /// Of those, how many the human overrode (polarity negative).
    pub negative_count: u64,
    /// Shadow firings that the user later matched manually (`matched_later_user_action`).
    pub shadow_matched: u64,
    /// Total shadow firings recorded for the rule.
    pub shadow_total: u64,
}

impl RuleOutcome {
    /// A zeroed outcome for `rule_id`/`rule_kind`, ready to accumulate counts into.
    #[must_use]
    pub fn new(rule_id: RuleId, rule_kind: RuleKind) -> Self {
        Self {
            rule_id,
            rule_kind,
            fire_count: 0,
            positive_count: 0,
            negative_count: 0,
            shadow_matched: 0,
            shadow_total: 0,
        }
    }

    /// Live precision: agreed / (agreed + overridden), or `None` with no live feedback.
    #[must_use]
    pub fn precision(&self) -> Option<f64> {
        let total = self.positive_count + self.negative_count;
        (total > 0).then(|| self.positive_count as f64 / total as f64)
    }

    /// Shadow precision: matched / total shadow firings, or `None` with no shadow data.
    #[must_use]
    pub fn shadow_precision(&self) -> Option<f64> {
        (self.shadow_total > 0).then(|| self.shadow_matched as f64 / self.shadow_total as f64)
    }

    /// Whether the rule looks healthy: it has live feedback and its precision clears `bar`.
    #[must_use]
    pub fn is_healthy(&self, bar: f64) -> bool {
        self.precision().is_some_and(|p| p >= bar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome() -> RuleOutcome {
        RuleOutcome {
            rule_id: RuleId::from("rule_1"),
            rule_kind: RuleKind::Action,
            fire_count: 10,
            positive_count: 8,
            negative_count: 2,
            shadow_matched: 3,
            shadow_total: 4,
        }
    }

    #[test]
    fn precision_is_agreement_over_total_live_feedback() {
        assert_eq!(outcome().precision(), Some(0.8));
        assert_eq!(outcome().shadow_precision(), Some(0.75));
        assert!(outcome().is_healthy(0.75));
        assert!(!outcome().is_healthy(0.9));
    }

    #[test]
    fn no_feedback_yields_no_precision() {
        let empty = RuleOutcome::new(RuleId::from("rule_2"), RuleKind::Classification);
        assert_eq!(empty.precision(), None);
        assert_eq!(empty.shadow_precision(), None);
        assert!(!empty.is_healthy(0.0), "no feedback is not 'healthy'");
    }
}
