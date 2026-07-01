//! The deterministic rule engine: interprets the JSON-AST condition language and the P2
//! action hierarchy with **no model inference** (determinism-first — a learned trait is a
//! model-free rule). It implements the `RuleEngine` port; `mailmate-core` depends on the
//! trait, never on this crate.
//!
//! The engine holds an immutable snapshot of evaluatable rules (fed by the rule repository
//! in production; by a fixture in tests). For each evaluation it:
//!
//! - skips rules that do not fire (draft/pending/disabled/retired/rejected),
//! - evaluates active and shadow rules' conditions against the field environment,
//! - applies active matches (ranked by hierarchy band — system-safety outranks human-hard
//!   outranks learned outranks AI-suggestion) and records shadow matches without applying,
//! - preserves the exact rule **version** that decided each match, so the decision is
//!   replayable and explainable.
//!
//! `explain` reconstructs a decision from an in-process record of the evaluation. In
//! production that record is reconstructed from `audit_log` / feedback rows (there is no
//! `decisions` table); the in-process map is the Phase-4 mechanism behind the same trait.

pub mod conflict;
pub mod evaluator;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use async_trait::async_trait;

use mailmate_common::error::RuleEngineError;
use mailmate_common::ids::DecisionId;
use mailmate_common::rules::evaluation::{
    AppliedEffect, DecisionExplanation, MatchedRule, RuleConflict, RuleEvaluationContext,
    RuleEvaluationResult, ShadowOutcome,
};
use mailmate_common::rules::rule::{EvaluatableRule, HierarchyBand, RuleDraft};
use mailmate_ports::rule_engine::RuleEngine;

use crate::evaluator::evaluate_condition;

/// The default, deterministic [`RuleEngine`] adapter.
///
/// The rule snapshot lives behind an [`RwLock`] so it can be hot-reloaded in place (via
/// [`reload`](Self::reload)) when the active/shadow set changes — e.g. the moment a human
/// activates a learned rule — without rebuilding the engine or restarting the host. The
/// in-process decision record is deliberately kept across a reload so a prior decision stays
/// explainable.
#[derive(Debug)]
pub struct DeterministicRuleEngine {
    rules: RwLock<Arc<Vec<EvaluatableRule>>>,
    decisions: Mutex<HashMap<DecisionId, RuleEvaluationResult>>,
}

impl DeterministicRuleEngine {
    /// Build an engine over a rule snapshot.
    #[must_use]
    pub fn new(rules: Vec<EvaluatableRule>) -> Self {
        Self {
            rules: RwLock::new(Arc::new(rules)),
            decisions: Mutex::new(HashMap::new()),
        }
    }

    /// A cheap, lock-free-after-clone handle to the current rule snapshot. Cloning the `Arc`
    /// under the read lock keeps the (sync) evaluation loop from holding the lock.
    fn snapshot(&self) -> Arc<Vec<EvaluatableRule>> {
        self.rules
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn record(&self, result: &RuleEvaluationResult) {
        self.decisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(result.decision_id.clone(), result.clone());
    }
}

#[async_trait]
impl RuleEngine for DeterministicRuleEngine {
    async fn evaluate(
        &self,
        context: RuleEvaluationContext,
    ) -> Result<RuleEvaluationResult, RuleEngineError> {
        let mut matched_rules = Vec::new();
        let mut applied_effects = Vec::new();
        let mut shadow_outcomes = Vec::new();

        let rules = self.snapshot();
        for rule in rules.iter() {
            // draft / pending / disabled / retired / rejected never fire.
            if !rule.status.is_evaluated() {
                continue;
            }
            if !evaluate_condition(&rule.version.condition, &context.fields) {
                continue;
            }

            let applies = rule.status.applies_effect();
            matched_rules.push(MatchedRule {
                rule_id: rule.rule_id.clone(),
                rule_version_id: rule.version.id.clone(),
                version_number: rule.version.version_number,
                band: rule.band,
                applied: applies,
            });

            if applies {
                applied_effects.push(AppliedEffect {
                    rule_id: rule.rule_id.clone(),
                    rule_version_id: rule.version.id.clone(),
                    band: rule.band,
                    effect: rule.version.effect.clone(),
                });
            } else {
                // Shadow rules record what they WOULD do, but never apply it.
                shadow_outcomes.push(ShadowOutcome {
                    rule_id: rule.rule_id.clone(),
                    rule_version_id: rule.version.id.clone(),
                    would_apply: rule.version.effect.clone(),
                });
            }
        }

        // Rank by hierarchy band (stable within a band): system-safety leads.
        matched_rules.sort_by_key(|m| (m.band.ordinal(), m.rule_id.as_str().to_owned()));
        applied_effects.sort_by_key(|e| e.band.ordinal());

        let result = RuleEvaluationResult {
            decision_id: context.decision_id,
            matched_rules,
            applied_effects,
            shadow_outcomes,
        };
        self.record(&result);
        Ok(result)
    }

    async fn explain(
        &self,
        decision_id: DecisionId,
    ) -> Result<DecisionExplanation, RuleEngineError> {
        let guard = self
            .decisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let result = guard
            .get(&decision_id)
            .ok_or_else(|| RuleEngineError::UnknownDecision(decision_id.to_string()))?;
        Ok(DecisionExplanation {
            decision_id: result.decision_id.clone(),
            matched_rules: result.matched_rules.clone(),
            applied_effects: result.applied_effects.clone(),
            shadow_outcomes: result.shadow_outcomes.clone(),
            narrative: build_narrative(result),
        })
    }

    async fn detect_conflicts(
        &self,
        candidate: RuleDraft,
    ) -> Result<Vec<RuleConflict>, RuleEngineError> {
        Ok(conflict::detect_conflicts(&candidate, &self.snapshot()))
    }

    /// Replace the rule snapshot in place — the hot-reload behind a just-activated (or
    /// status-changed) rule taking effect on the next evaluation, without a host restart. The
    /// decision record is preserved, so explanations of earlier decisions still resolve.
    fn reload(&self, rules: Vec<EvaluatableRule>) {
        *self.rules.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(rules);
    }
}

fn band_label(band: HierarchyBand) -> &'static str {
    match band {
        HierarchyBand::SystemSafety => "system_safety",
        HierarchyBand::HumanHard => "human_hard",
        HierarchyBand::LearnedActive => "learned_active",
        HierarchyBand::AgentShadow => "agent_shadow",
        HierarchyBand::AiSuggestion => "ai_suggestion",
        HierarchyBand::DefaultFallback => "default_fallback",
    }
}

fn build_narrative(result: &RuleEvaluationResult) -> String {
    if result.matched_rules.is_empty() {
        return "No rules matched; conservative default applies.".to_owned();
    }
    let lines: Vec<String> = result
        .matched_rules
        .iter()
        .map(|m| {
            let verb = if m.applied {
                "applied"
            } else {
                "recorded (shadow)"
            };
            format!(
                "{verb} rule {} v{} [{}]",
                m.rule_id,
                m.version_number,
                band_label(m.band)
            )
        })
        .collect();
    lines.join("; ")
}

/// Returns this crate's package name for smoke tests.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use futures::executor::block_on;
    use mailmate_common::ids::{DecisionId, RuleId, RuleVersionId};
    use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
    use mailmate_common::rules::effect::RuleEffect;
    use mailmate_common::rules::rule::{RiskLevel, RuleKind, RuleScope, RuleStatus, RuleVersion};

    #[test]
    fn crate_name_is_available() {
        assert_eq!(super::crate_name(), "mailmate-rules");
    }

    fn ctx(decision: &str, domain: &str) -> RuleEvaluationContext {
        let mut fields = BTreeMap::new();
        fields.insert(
            "sender_domain".to_owned(),
            FieldValue::Text(domain.to_owned()),
        );
        RuleEvaluationContext {
            decision_id: DecisionId::from(decision),
            fields,
        }
    }

    fn active_filing_rule(domain: &str, folder: &str) -> EvaluatableRule {
        EvaluatableRule {
            rule_id: RuleId::from("rule_live"),
            kind: RuleKind::Action,
            scope: RuleScope::Domain,
            band: HierarchyBand::LearnedActive,
            status: RuleStatus::Active,
            version: RuleVersion {
                id: RuleVersionId::from("rv_live"),
                version_number: 1,
                condition: Condition::Predicate(Predicate {
                    field: "sender_domain".to_owned(),
                    op: Operator::Eq,
                    value: FieldValue::Text(domain.to_owned()),
                }),
                effect: RuleEffect {
                    move_to: Some(folder.to_owned()),
                    ..RuleEffect::new()
                },
                risk_level: RiskLevel::Low,
            },
        }
    }

    #[test]
    fn reload_swaps_the_rule_snapshot_and_preserves_recorded_decisions() {
        // The hot-reload primitive behind a just-activated rule firing without a host restart.
        let engine = DeterministicRuleEngine::new(vec![]);

        // A decision over the empty rule set applies nothing.
        let before = block_on(engine.evaluate(ctx("dec_1", "stripe.com"))).unwrap();
        assert!(before.applied_effects.is_empty(), "no rules yet");

        // Activate a rule by reloading the snapshot in place.
        engine.reload(vec![active_filing_rule("stripe.com", "Receipts")]);

        // A NEW evaluation now applies the freshly-activated rule.
        let after = block_on(engine.evaluate(ctx("dec_2", "stripe.com"))).unwrap();
        assert_eq!(after.applied_effects.len(), 1, "the reloaded rule fires");
        assert_eq!(
            after.applied_effects[0].effect.move_to.as_deref(),
            Some("Receipts")
        );

        // The decision recorded BEFORE the reload is still explainable — reload swaps the rules
        // in place, it does not discard the engine's decision record.
        let explained = block_on(engine.explain(DecisionId::from("dec_1"))).unwrap();
        assert_eq!(explained.decision_id, DecisionId::from("dec_1"));
    }
}
