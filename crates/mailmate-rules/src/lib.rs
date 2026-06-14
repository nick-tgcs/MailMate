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
use std::sync::{Mutex, PoisonError};

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
#[derive(Debug)]
pub struct DeterministicRuleEngine {
    rules: Vec<EvaluatableRule>,
    decisions: Mutex<HashMap<DecisionId, RuleEvaluationResult>>,
}

impl DeterministicRuleEngine {
    /// Build an engine over an immutable rule snapshot.
    #[must_use]
    pub fn new(rules: Vec<EvaluatableRule>) -> Self {
        Self {
            rules,
            decisions: Mutex::new(HashMap::new()),
        }
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

        for rule in &self.rules {
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
        Ok(conflict::detect_conflicts(&candidate, &self.rules))
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
    #[test]
    fn crate_name_is_available() {
        assert_eq!(super::crate_name(), "mailmate-rules");
    }
}
