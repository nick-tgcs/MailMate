//! The rule-engine port: evaluate the rule hierarchy, explain a decision, and detect
//! conflicts in a candidate rule.
//!
//! Like the policy guard, the rule engine is deterministic domain logic behind a port —
//! the core names only this trait. The default adapter
//! (`mailmate-rules::DeterministicRuleEngine`) interprets the JSON-AST condition language
//! with no model inference (determinism-first: a learned trait is a model-free rule).

use async_trait::async_trait;

use mailmate_common::error::RuleEngineError;
use mailmate_common::rules::evaluation::{
    DecisionExplanation, RuleConflict, RuleEvaluationContext, RuleEvaluationResult,
};
use mailmate_common::rules::rule::RuleDraft;

/// Evaluates rules against a message context and reasons about candidate rules.
#[async_trait]
pub trait RuleEngine: Send + Sync {
    /// Evaluate the rule set against `context`, returning the matched rules, the applied
    /// effects (active rules, ranked by hierarchy band), and the shadow outcomes.
    ///
    /// # Errors
    /// [`RuleEngineError`] on an invalid condition or adapter failure.
    async fn evaluate(
        &self,
        context: RuleEvaluationContext,
    ) -> Result<RuleEvaluationResult, RuleEngineError>;

    /// Reconstruct an explanation of a prior decision — the matched rules, their exact
    /// versions, the applied effects, and a narrative.
    ///
    /// # Errors
    /// [`RuleEngineError::UnknownDecision`] if the engine has no record of the decision.
    async fn explain(
        &self,
        decision_id: mailmate_common::ids::DecisionId,
    ) -> Result<DecisionExplanation, RuleEngineError>;

    /// Detect conflicts between a candidate rule and the existing rules.
    ///
    /// # Errors
    /// [`RuleEngineError`] on an invalid candidate or adapter failure.
    async fn detect_conflicts(
        &self,
        candidate: RuleDraft,
    ) -> Result<Vec<RuleConflict>, RuleEngineError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn RuleEngine) {}
        let _ = takes as fn(&dyn RuleEngine);
    }
}
