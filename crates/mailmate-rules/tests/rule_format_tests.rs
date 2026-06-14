//! End-to-end-equivalent: parse the spec's example rule conditions/effects from JSON and
//! run them through the engine against a realistic field environment — validating the
//! JSON-AST condition format deliverable from authoring text to applied effect.

use futures::executor::block_on;
use serde_json::json;

use mailmate_common::ids::{DecisionId, RuleId, RuleVersionId};
use mailmate_common::rules::condition::{Condition, FieldValue};
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::evaluation::RuleEvaluationContext;
use mailmate_common::rules::rule::{
    EvaluatableRule, HierarchyBand, RiskLevel, RuleKind, RuleScope, RuleStatus, RuleVersion,
};
use mailmate_ports::rule_engine::RuleEngine;
use mailmate_rules::DeterministicRuleEngine;

fn rule_from(condition: Condition, effect: RuleEffect, status: RuleStatus) -> EvaluatableRule {
    EvaluatableRule {
        rule_id: RuleId::from("rule_example"),
        kind: RuleKind::Action,
        scope: RuleScope::Global,
        band: HierarchyBand::LearnedActive,
        status,
        version: RuleVersion {
            id: RuleVersionId::from("rv_example"),
            version_number: 1,
            condition,
            effect,
            risk_level: RiskLevel::Low,
        },
    }
}

#[test]
fn safe_tagging_example_parses_and_applies() {
    // From architecture.md "Example rule: safe tagging".
    let condition: Condition = serde_json::from_value(json!({
        "all": [
            { "field": "sender_domain", "op": "in", "value": ["github.com", "stripe.com", "figma.com"] },
            { "field": "subject_normalized", "op": "contains_any", "value": ["receipt", "invoice", "payment"] }
        ]
    }))
    .unwrap();
    let effect: RuleEffect =
        serde_json::from_value(json!({ "tag": ["receipt"], "priority": "normal" })).unwrap();

    let engine =
        DeterministicRuleEngine::new(vec![rule_from(condition, effect, RuleStatus::Active)]);

    // A matching message: GitHub receipt.
    let ctx = RuleEvaluationContext::new(DecisionId::fresh())
        .with("sender_domain", FieldValue::Text("github.com".to_owned()))
        .with(
            "subject_normalized",
            FieldValue::Text("your github receipt".to_owned()),
        );
    let result = block_on(engine.evaluate(ctx)).unwrap();
    assert_eq!(result.applied_effects.len(), 1);
    assert_eq!(
        result.applied_effects[0].effect.tag,
        vec!["receipt".to_owned()]
    );

    // A non-matching message: same subject, different sender domain.
    let ctx_miss = RuleEvaluationContext::new(DecisionId::fresh())
        .with("sender_domain", FieldValue::Text("evil.example".to_owned()))
        .with(
            "subject_normalized",
            FieldValue::Text("your receipt".to_owned()),
        );
    assert!(block_on(engine.evaluate(ctx_miss))
        .unwrap()
        .applied_effects
        .is_empty());
}

#[test]
fn financial_review_example_parses_with_require_review_effect() {
    // From architecture.md "Example rule: financial review requirement".
    let condition: Condition = serde_json::from_value(json!({
        "any": [
            { "field": "classification.labels", "op": "contains", "value": "financial" },
            { "field": "subject_normalized", "op": "contains_any", "value": ["bank", "payment", "wire", "invoice", "tax"] }
        ]
    }))
    .unwrap();
    let effect: RuleEffect =
        serde_json::from_value(json!({ "require_review_for": ["move", "mark_junk"] })).unwrap();
    assert_eq!(
        effect.require_review_for,
        vec!["move".to_owned(), "mark_junk".to_owned()]
    );

    let engine =
        DeterministicRuleEngine::new(vec![rule_from(condition, effect, RuleStatus::Active)]);
    let ctx = RuleEvaluationContext::new(DecisionId::fresh()).with(
        "classification.labels",
        FieldValue::TextSet(vec!["financial".to_owned()]),
    );
    let result = block_on(engine.evaluate(ctx)).unwrap();
    assert_eq!(
        result.applied_effects[0].effect.require_review_for,
        vec!["move".to_owned(), "mark_junk".to_owned()]
    );
}

#[test]
fn shadow_example_records_a_would_move_without_applying() {
    // From architecture.md "Example shadow rule" (uses the `>=` symbolic operator).
    let condition: Condition = serde_json::from_value(json!({
        "all": [
            { "field": "subject_normalized", "op": "contains_any", "value": ["weekly update", "status update"] },
            { "field": "sender_seen_count", "op": ">=", "value": 5 }
        ]
    }))
    .unwrap();
    let effect: RuleEffect = serde_json::from_value(json!({ "move": "Projects/Updates" })).unwrap();

    let engine =
        DeterministicRuleEngine::new(vec![rule_from(condition, effect, RuleStatus::ShadowMode)]);
    let ctx = RuleEvaluationContext::new(DecisionId::fresh())
        .with(
            "subject_normalized",
            FieldValue::Text("weekly update from the team".to_owned()),
        )
        .with("sender_seen_count", FieldValue::Int(8));
    let result = block_on(engine.evaluate(ctx)).unwrap();

    assert!(
        result.applied_effects.is_empty(),
        "shadow rule applies nothing"
    );
    assert_eq!(
        result.shadow_outcomes[0].would_apply.move_to.as_deref(),
        Some("Projects/Updates")
    );
}
