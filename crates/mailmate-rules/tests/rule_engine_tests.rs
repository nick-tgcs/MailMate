//! The required rule-engine test cases (architecture.md → Testing Strategy → Rule engine
//! tests), driven through the public `RuleEngine` port.

use futures::executor::block_on;

use mailmate_common::ids::{DecisionId, RuleId, RuleVersionId};
use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::evaluation::RuleEvaluationContext;
use mailmate_common::rules::rule::{
    EvaluatableRule, HierarchyBand, RiskLevel, RuleDraft, RuleKind, RuleScope, RuleStatus,
    RuleVersion,
};
use mailmate_ports::rule_engine::RuleEngine;
use mailmate_rules::DeterministicRuleEngine;

/// An always-true condition (an empty `all` holds vacuously).
fn always() -> Condition {
    Condition::All { all: vec![] }
}

fn tag_effect(tag: &str) -> RuleEffect {
    RuleEffect {
        tag: vec![tag.to_owned()],
        ..RuleEffect::new()
    }
}

fn rule(
    id: &str,
    band: HierarchyBand,
    status: RuleStatus,
    condition: Condition,
    effect: RuleEffect,
) -> EvaluatableRule {
    EvaluatableRule {
        rule_id: RuleId::from(id),
        kind: RuleKind::Action,
        scope: RuleScope::Global,
        band,
        status,
        version: RuleVersion {
            id: RuleVersionId::from(format!("rv_{id}")),
            version_number: 1,
            condition,
            effect,
            risk_level: RiskLevel::Low,
        },
    }
}

fn context() -> RuleEvaluationContext {
    RuleEvaluationContext::new(DecisionId::fresh())
}

fn evaluate(
    rules: Vec<EvaluatableRule>,
) -> mailmate_common::rules::evaluation::RuleEvaluationResult {
    block_on(DeterministicRuleEngine::new(rules).evaluate(context())).unwrap()
}

#[test]
fn system_safety_rules_outrank_all_others() {
    let result = evaluate(vec![
        rule(
            "learned",
            HierarchyBand::LearnedActive,
            RuleStatus::Active,
            always(),
            tag_effect("l"),
        ),
        rule(
            "safety",
            HierarchyBand::SystemSafety,
            RuleStatus::Active,
            always(),
            tag_effect("s"),
        ),
        rule(
            "human",
            HierarchyBand::HumanHard,
            RuleStatus::Active,
            always(),
            tag_effect("h"),
        ),
    ]);
    assert_eq!(result.winning_band(), Some(HierarchyBand::SystemSafety));
    assert_eq!(result.applied_effects[0].rule_id, RuleId::from("safety"));
    // matched_rules is ranked the same way.
    assert_eq!(result.matched_rules[0].band, HierarchyBand::SystemSafety);
}

#[test]
fn human_hard_rules_outrank_learned_rules() {
    let result = evaluate(vec![
        rule(
            "learned",
            HierarchyBand::LearnedActive,
            RuleStatus::Active,
            always(),
            tag_effect("l"),
        ),
        rule(
            "human",
            HierarchyBand::HumanHard,
            RuleStatus::Active,
            always(),
            tag_effect("h"),
        ),
    ]);
    assert_eq!(result.winning_band(), Some(HierarchyBand::HumanHard));
}

#[test]
fn learned_active_rules_outrank_ai_suggestions() {
    let result = evaluate(vec![
        rule(
            "ai",
            HierarchyBand::AiSuggestion,
            RuleStatus::Active,
            always(),
            tag_effect("a"),
        ),
        rule(
            "learned",
            HierarchyBand::LearnedActive,
            RuleStatus::Active,
            always(),
            tag_effect("l"),
        ),
    ]);
    assert_eq!(result.winning_band(), Some(HierarchyBand::LearnedActive));
}

#[test]
fn shadow_rules_record_outcomes_but_do_not_apply_actions() {
    let result = evaluate(vec![rule(
        "shadow",
        HierarchyBand::AgentShadow,
        RuleStatus::ShadowMode,
        always(),
        RuleEffect {
            move_to: Some("Projects/Updates".to_owned()),
            ..RuleEffect::new()
        },
    )]);
    assert!(
        result.applied_effects.is_empty(),
        "shadow rules apply nothing"
    );
    assert_eq!(result.shadow_outcomes.len(), 1);
    assert_eq!(
        result.shadow_outcomes[0].would_apply.move_to.as_deref(),
        Some("Projects/Updates")
    );
    assert_eq!(result.matched_rules.len(), 1);
    assert!(!result.matched_rules[0].applied);
}

#[test]
fn disabled_retired_rejected_draft_pending_rules_do_not_fire() {
    let result = evaluate(vec![
        rule(
            "disabled",
            HierarchyBand::HumanHard,
            RuleStatus::Disabled,
            always(),
            tag_effect("d"),
        ),
        rule(
            "retired",
            HierarchyBand::HumanHard,
            RuleStatus::Retired,
            always(),
            tag_effect("r"),
        ),
        rule(
            "rejected",
            HierarchyBand::HumanHard,
            RuleStatus::Rejected,
            always(),
            tag_effect("x"),
        ),
        rule(
            "draft",
            HierarchyBand::HumanHard,
            RuleStatus::Draft,
            always(),
            tag_effect("f"),
        ),
        rule(
            "pending",
            HierarchyBand::HumanHard,
            RuleStatus::PendingHumanReview,
            always(),
            tag_effect("p"),
        ),
    ]);
    assert!(
        result.matched_rules.is_empty(),
        "none of these statuses fire"
    );
    assert!(result.applied_effects.is_empty());
    assert!(result.shadow_outcomes.is_empty());
}

#[test]
fn conflict_detection_catches_contradictory_effects() {
    let condition = Condition::Predicate(Predicate {
        field: "sender_domain".to_owned(),
        op: Operator::Eq,
        value: FieldValue::Text("acme.example".to_owned()),
    });
    let existing = rule(
        "filer",
        HierarchyBand::HumanHard,
        RuleStatus::Active,
        condition.clone(),
        RuleEffect {
            move_to: Some("FolderA".to_owned()),
            ..RuleEffect::new()
        },
    );
    let engine = DeterministicRuleEngine::new(vec![existing]);
    let candidate = RuleDraft {
        kind: RuleKind::Action,
        scope: RuleScope::Global,
        condition,
        effect: RuleEffect {
            move_to: Some("FolderB".to_owned()),
            ..RuleEffect::new()
        },
    };
    let conflicts = block_on(engine.detect_conflicts(candidate)).unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(
        conflicts[0].kind,
        mailmate_common::rules::evaluation::ConflictKind::ContradictoryEffect
    );
    assert_eq!(conflicts[0].existing_rule_id, Some(RuleId::from("filer")));
}

#[test]
fn rule_version_used_in_a_decision_is_preserved() {
    let mut versioned = rule(
        "v",
        HierarchyBand::HumanHard,
        RuleStatus::Active,
        always(),
        tag_effect("t"),
    );
    versioned.version.id = RuleVersionId::from("rv_pinned_7");
    versioned.version.version_number = 7;

    let engine = DeterministicRuleEngine::new(vec![versioned]);
    let ctx = context();
    let decision_id = ctx.decision_id.clone();
    let result = block_on(engine.evaluate(ctx)).unwrap();

    assert_eq!(
        result.matched_rules[0].rule_version_id,
        RuleVersionId::from("rv_pinned_7")
    );
    assert_eq!(result.matched_rules[0].version_number, 7);

    // The same pinned version survives into the reconstructed explanation.
    let explanation = block_on(engine.explain(decision_id)).unwrap();
    assert_eq!(
        explanation.matched_rules[0].rule_version_id,
        RuleVersionId::from("rv_pinned_7")
    );
    assert!(explanation.narrative.contains("rv_pinned_7") || explanation.narrative.contains("v7"));
}

#[test]
fn explain_rejects_an_unknown_decision() {
    let engine = DeterministicRuleEngine::new(vec![]);
    let err = block_on(engine.explain(DecisionId::from("dec_never"))).unwrap_err();
    assert!(
        matches!(
            err,
            mailmate_common::error::RuleEngineError::UnknownDecision(_)
        ),
        "got {err:?}"
    );
}
