//! Pipeline-2 action planning: an active action rule matching the classification produces a
//! candidate plan; a classification that needs review surfaces a `RequireReview`; an
//! unstored message is refused. The candidate plan is *untrusted* — the policy guard runs
//! later (see `end_to_end.rs`).

use std::sync::Arc;

use futures::executor::block_on;

use mailmate_common::action::ProposedAction;
use mailmate_common::classification::{Classification, ClassificationProvenance, Priority};
use mailmate_common::error::ActionPlanningError;
use mailmate_common::features::FeatureVector;
use mailmate_common::ids::{AccountId, DecisionId, FolderId, MessageId, RuleId, RuleVersionId};
use mailmate_common::mail::{MessageData, MessageHeaders};
use mailmate_common::planning::ActionPlanningInput;
use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::rule::{
    EvaluatableRule, HierarchyBand, RiskLevel, RuleKind, RuleScope, RuleStatus, RuleVersion,
};
use mailmate_ports::action_planner::ActionPlanner;

use mailmate_planner::DefaultActionPlanner;
use mailmate_rules::DeterministicRuleEngine;

fn message(id: Option<&str>) -> MessageData {
    MessageData {
        id: id.map(MessageId::from),
        client_message_id: "1".to_owned(),
        account_id: AccountId::from("acct_a"),
        folder_id: FolderId::from("folder_inbox"),
        thread_id: None,
        headers: MessageHeaders {
            from: "billing@vendor.example".to_owned(),
            subject: "Your receipt".to_owned(),
            ..MessageHeaders::default()
        },
        body_text: None,
        attachments: vec![],
        remote_content_loaded: false,
    }
}

fn classification(labels: &[&str], needs_review: bool) -> Classification {
    Classification {
        decision_id: DecisionId::from("dec_1"),
        labels: labels.iter().map(|s| (*s).to_owned()).collect(),
        spam_score: 0.0,
        phishing_score: 0.0,
        priority: Priority::Normal,
        needs_review,
        provenance: ClassificationProvenance::tier2("scripted-v1", false),
    }
}

fn action_rule(condition: Condition, effect: RuleEffect, status: RuleStatus) -> EvaluatableRule {
    EvaluatableRule {
        rule_id: RuleId::from("rule_receipts"),
        kind: RuleKind::Action,
        scope: RuleScope::Global,
        band: HierarchyBand::LearnedActive,
        status,
        version: RuleVersion {
            id: RuleVersionId::from("rv_receipts"),
            version_number: 1,
            condition,
            effect,
            risk_level: RiskLevel::Low,
        },
    }
}

/// `classification.labels contains "receipt"` → tag + move.
fn receipt_rule(status: RuleStatus) -> EvaluatableRule {
    action_rule(
        Condition::Predicate(Predicate {
            field: "classification.labels".to_owned(),
            op: Operator::Contains,
            value: FieldValue::Text("receipt".to_owned()),
        }),
        RuleEffect {
            tag: vec!["receipt".to_owned()],
            move_to: Some("Receipts".to_owned()),
            ..RuleEffect::new()
        },
        status,
    )
}

#[test]
fn an_active_action_rule_produces_a_candidate_plan() {
    let planner =
        DefaultActionPlanner::new(Arc::new(DeterministicRuleEngine::new(vec![receipt_rule(
            RuleStatus::Active,
        )])));
    let input = ActionPlanningInput::new_mail(
        message(Some("msg_1")),
        classification(&["receipt"], false),
        FeatureVector::new(),
    );

    let plan = block_on(planner.plan(input)).unwrap();
    assert_eq!(plan.message_id, Some(MessageId::from("msg_1")));
    assert_eq!(plan.actions.len(), 2);
    assert!(plan
        .actions
        .iter()
        .any(|a| matches!(a, ProposedAction::Tag { tag, .. } if tag == "receipt")));
    assert!(plan.actions.iter().any(
        |a| matches!(a, ProposedAction::Move { to_folder, .. } if to_folder.as_str() == "Receipts")
    ));
    // Every candidate is safely applicable (structural floor beneath the policy guard).
    assert!(plan.actions.iter().all(|a| a.to_planned().is_some()));
}

#[test]
fn a_shadow_rule_does_not_produce_an_action() {
    // A shadow-mode rule is evaluated but never applies its effect.
    let planner =
        DefaultActionPlanner::new(Arc::new(DeterministicRuleEngine::new(vec![receipt_rule(
            RuleStatus::ShadowMode,
        )])));
    let input = ActionPlanningInput::new_mail(
        message(Some("msg_1")),
        classification(&["receipt"], false),
        FeatureVector::new(),
    );
    let plan = block_on(planner.plan(input)).unwrap();
    assert!(plan.actions.is_empty(), "a shadow rule applies no action");
}

#[test]
fn a_non_matching_classification_yields_an_empty_plan() {
    let planner =
        DefaultActionPlanner::new(Arc::new(DeterministicRuleEngine::new(vec![receipt_rule(
            RuleStatus::Active,
        )])));
    let input = ActionPlanningInput::new_mail(
        message(Some("msg_1")),
        classification(&["newsletter"], false),
        FeatureVector::new(),
    );
    let plan = block_on(planner.plan(input)).unwrap();
    assert!(
        plan.actions.is_empty(),
        "no rule matched → conservative empty plan"
    );
}

#[test]
fn a_needs_review_classification_surfaces_a_require_review_action() {
    // No rules at all, but the cascade could not clear the message → surface review.
    let planner = DefaultActionPlanner::new(Arc::new(DeterministicRuleEngine::new(vec![])));
    let input = ActionPlanningInput::new_mail(
        message(Some("msg_1")),
        classification(&["needs_review"], true),
        FeatureVector::new(),
    );
    let plan = block_on(planner.plan(input)).unwrap();
    assert_eq!(plan.actions.len(), 1);
    assert!(matches!(
        plan.actions[0],
        ProposedAction::RequireReview { .. }
    ));
}

#[test]
fn planning_an_unstored_message_is_refused() {
    let planner = DefaultActionPlanner::new(Arc::new(DeterministicRuleEngine::new(vec![])));
    let input = ActionPlanningInput::new_mail(
        message(None),
        classification(&["receipt"], false),
        FeatureVector::new(),
    );
    let err = block_on(planner.plan(input)).unwrap_err();
    assert!(
        matches!(err, ActionPlanningError::MissingMessageId),
        "got {err:?}"
    );
}
