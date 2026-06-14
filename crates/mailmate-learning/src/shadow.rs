//! The crystallization gate: back-test a candidate rule against history.
//!
//! This is promotion-gate step 2 (*back-test it against history*): replay the candidate
//! over the recorded decisions it claims to explain and check it **reproduces the user's
//! actual past decisions at a precision bar with enough support**. It runs entirely
//! in-memory over the deterministic rule engine — no model, no persistence — so the same
//! candidate + history always yields the same verdict. A candidate that cannot clear the
//! bar is not eligible for promotion (it stays model-assisted; MailMate does not ship a
//! low-precision "learned" rule).

use std::collections::BTreeMap;

use mailmate_common::error::LearningError;
use mailmate_common::ids::{DecisionId, MessageId, RuleId, RuleVersionId};
use mailmate_common::rules::condition::FieldValue;
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::evaluation::RuleEvaluationContext;
use mailmate_common::rules::rule::{
    EvaluatableRule, HierarchyBand, RuleDraft, RuleStatus, RuleVersion,
};
use mailmate_ports::rule_engine::RuleEngine;
use mailmate_rules::DeterministicRuleEngine;

/// One historical decision the candidate is replayed against: the deterministic field
/// environment at the time, and the effect the user actually applied.
#[derive(Clone, Debug)]
pub struct HistoricalExample {
    /// The message the decision concerned, if any.
    pub message_id: Option<MessageId>,
    /// The field environment (`sender_domain`, …) the candidate is evaluated over.
    pub fields: BTreeMap<String, FieldValue>,
    /// The effect the user actually applied (move/labels/tags).
    pub actual_effect: RuleEffect,
}

impl HistoricalExample {
    /// A convenience example keyed on a single `sender_domain` field.
    #[must_use]
    pub fn from_domain(domain: &str, actual_effect: RuleEffect) -> Self {
        let mut fields = BTreeMap::new();
        fields.insert(
            "sender_domain".to_owned(),
            FieldValue::Text(domain.to_owned()),
        );
        Self {
            message_id: None,
            fields,
            actual_effect,
        }
    }
}

/// The back-test verdict: how often the candidate fired, and how often its effect agreed
/// with what the user actually did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShadowReport {
    /// How many historical examples the candidate fired on.
    pub fires: usize,
    /// Of those, how many its effect agreed with the user's actual action.
    pub correct: usize,
}

impl ShadowReport {
    /// The supporting count — how many decisions the candidate actually fired on.
    #[must_use]
    pub fn support(self) -> usize {
        self.fires
    }

    /// Precision over the firings (`correct / fires`), or `None` if it never fired.
    #[must_use]
    pub fn precision(self) -> Option<f64> {
        (self.fires > 0).then(|| self.correct as f64 / self.fires as f64)
    }

    /// Whether the candidate clears the promotion bar: precision ≥ `bar` AND it fired on at
    /// least `min_support` decisions (a high-precision rule that fired twice is not yet
    /// trustworthy).
    #[must_use]
    pub fn is_eligible(self, bar: f64, min_support: usize) -> bool {
        self.support() >= min_support && self.precision().is_some_and(|p| p >= bar)
    }
}

/// Whether every effect the candidate would impose is one the user actually did — the
/// agreement test behind precision. A candidate that would move to `F` is correct only if
/// the user moved to `F`; a candidate that would set label `L` is correct only if the user
/// applied `L`.
#[must_use]
pub fn effect_agrees(candidate: &RuleEffect, actual: &RuleEffect) -> bool {
    let move_ok = candidate.move_to.is_none() || candidate.move_to == actual.move_to;
    let labels_ok = candidate
        .set_labels
        .iter()
        .all(|l| actual.set_labels.contains(l));
    let tags_ok = candidate.tag.iter().all(|t| actual.tag.contains(t));
    let junk_ok = candidate.mark_junk.is_none() || candidate.mark_junk == actual.mark_junk;
    move_ok && labels_ok && tags_ok && junk_ok
}

/// Lift a candidate draft into a shadow-mode evaluatable rule (a stable synthetic id, so
/// the back-test is reproducible).
fn lift_candidate(draft: &RuleDraft) -> EvaluatableRule {
    EvaluatableRule {
        rule_id: RuleId::from("rule_candidate"),
        kind: draft.kind,
        scope: draft.scope,
        band: HierarchyBand::AgentShadow,
        status: RuleStatus::ShadowMode,
        version: RuleVersion {
            id: RuleVersionId::from("rv_candidate"),
            version_number: 1,
            condition: draft.condition.clone(),
            effect: draft.effect.clone(),
            risk_level: mailmate_common::rules::rule::RiskLevel::Low,
        },
    }
}

/// Back-test `candidate` over `history`, returning how often it fired and agreed with the
/// user. The candidate is evaluated as a shadow rule (it records what it *would* do, never
/// applying), exactly as a promoted shadow rule would behave at runtime.
///
/// # Errors
/// [`LearningError::Rules`] if the rule engine fails to evaluate a context.
pub async fn back_test(
    candidate: &RuleDraft,
    history: &[HistoricalExample],
) -> Result<ShadowReport, LearningError> {
    let engine = DeterministicRuleEngine::new(vec![lift_candidate(candidate)]);
    let mut fires = 0usize;
    let mut correct = 0usize;
    for example in history {
        let context = RuleEvaluationContext {
            decision_id: DecisionId::fresh(),
            fields: example.fields.clone(),
        };
        let result = engine.evaluate(context).await?;
        // A shadow rule that matches records a shadow outcome (it never applies).
        if !result.shadow_outcomes.is_empty() {
            fires += 1;
            if effect_agrees(&candidate.effect, &example.actual_effect) {
                correct += 1;
            }
        }
    }
    Ok(ShadowReport { fires, correct })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use mailmate_common::rules::condition::{Condition, Operator, Predicate};
    use mailmate_common::rules::rule::{RuleKind, RuleScope};

    fn move_to(folder: &str) -> RuleEffect {
        RuleEffect {
            move_to: Some(folder.to_owned()),
            ..RuleEffect::new()
        }
    }

    fn candidate(folder: &str) -> RuleDraft {
        RuleDraft {
            kind: RuleKind::Action,
            scope: RuleScope::Domain,
            condition: Condition::Predicate(Predicate {
                field: "sender_domain".to_owned(),
                op: Operator::Eq,
                value: FieldValue::Text("stripe.com".to_owned()),
            }),
            effect: move_to(folder),
        }
    }

    #[test]
    fn perfect_history_clears_the_bar() {
        let history = vec![
            HistoricalExample::from_domain("stripe.com", move_to("Receipts")),
            HistoricalExample::from_domain("stripe.com", move_to("Receipts")),
            HistoricalExample::from_domain("stripe.com", move_to("Receipts")),
            // A non-matching domain: the candidate does not fire, so it is not counted.
            HistoricalExample::from_domain("other.com", move_to("Inbox")),
        ];
        let report = block_on(back_test(&candidate("Receipts"), &history)).unwrap();
        assert_eq!(report.fires, 3, "fired only on the matching domain");
        assert_eq!(report.correct, 3);
        assert_eq!(report.precision(), Some(1.0));
        assert!(report.is_eligible(0.9, 3));
    }

    #[test]
    fn a_disagreeing_history_fails_the_precision_bar() {
        let history = vec![
            HistoricalExample::from_domain("stripe.com", move_to("Receipts")),
            // The user actually filed these elsewhere — the candidate would mis-file them.
            HistoricalExample::from_domain("stripe.com", move_to("Personal")),
            HistoricalExample::from_domain("stripe.com", move_to("Personal")),
        ];
        let report = block_on(back_test(&candidate("Receipts"), &history)).unwrap();
        assert_eq!(report.fires, 3);
        assert_eq!(report.correct, 1);
        assert_eq!(report.precision(), Some(1.0 / 3.0));
        assert!(!report.is_eligible(0.9, 2), "low precision is not eligible");
    }

    #[test]
    fn high_precision_but_thin_support_is_not_eligible() {
        let history = vec![HistoricalExample::from_domain(
            "stripe.com",
            move_to("Receipts"),
        )];
        let report = block_on(back_test(&candidate("Receipts"), &history)).unwrap();
        assert_eq!(report.precision(), Some(1.0));
        assert!(
            !report.is_eligible(0.9, 3),
            "one matching decision is not enough support"
        );
    }

    #[test]
    fn never_firing_has_no_precision() {
        let history = vec![HistoricalExample::from_domain("other.com", move_to("X"))];
        let report = block_on(back_test(&candidate("Receipts"), &history)).unwrap();
        assert_eq!(report.fires, 0);
        assert_eq!(report.precision(), None);
        assert!(!report.is_eligible(0.0, 0));
    }

    #[test]
    fn effect_agreement_requires_candidate_effects_to_be_a_subset() {
        assert!(effect_agrees(&move_to("R"), &move_to("R")));
        assert!(!effect_agrees(&move_to("R"), &move_to("Q")));
        // A label candidate agrees when the user applied that label among possibly others.
        let cand = RuleEffect {
            set_labels: vec!["phishing".to_owned()],
            ..RuleEffect::new()
        };
        let actual = RuleEffect {
            set_labels: vec!["phishing".to_owned(), "external".to_owned()],
            ..RuleEffect::new()
        };
        assert!(effect_agrees(&cand, &actual));
    }
}
