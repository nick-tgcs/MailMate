//! The full two-pipeline flow through the **core** `PlanningService`, driven by the **real**
//! adapters — the cascade (`CascadeClassifier`), the planner (`DefaultActionPlanner`), and the
//! hard policy guard (`HardPolicyGuard`) — with a deterministic mock provider for Tier 3.
//!
//! Covers the architecture's required integration cases:
//!   * "Classify message end-to-end through Rust core using mock provider."
//!   * "Activate rule and produce action plan."
//!   * "Policy guard blocks [auto-applying] an unsafe action from an otherwise matching rule."
//!   * Fully functional with **zero providers** (the Tier-2 path).

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::executor::block_on;
use serde_json::json;

use mailmate_common::classification::ClassificationTier;
use mailmate_common::error::MlError;
use mailmate_common::features::{CalibratedScores, FeatureVector, LabeledExample};
use mailmate_common::ids::{AccountId, FolderId, MessageId, RuleId, RuleVersionId};
use mailmate_common::mail::{MessageData, MessageHeaders};
use mailmate_common::policy::TriggerKind;
use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::rule::{
    EvaluatableRule, HierarchyBand, RiskLevel, RuleKind, RuleScope, RuleStatus, RuleVersion,
};
use mailmate_ports::tier2_classifier::Tier2Classifier;

use mailmate_ai::providers::MockProvider;
use mailmate_core::PlanningService;
use mailmate_planner::{CascadeClassifier, DefaultActionPlanner};
use mailmate_policy::HardPolicyGuard;
use mailmate_rules::DeterministicRuleEngine;
use mailmate_test_support::fakes::StubFeatureExtractor;

/// A Tier-2 classifier returning a fixed, scripted score map.
struct ScriptedTier2 {
    scores: BTreeMap<String, f64>,
}

impl ScriptedTier2 {
    fn new(pairs: &[(&str, f64)]) -> Self {
        Self {
            scores: pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect(),
        }
    }
}

#[async_trait]
impl Tier2Classifier for ScriptedTier2 {
    async fn predict(&self, _features: FeatureVector) -> Result<CalibratedScores, MlError> {
        Ok(CalibratedScores {
            scores: self.scores.clone(),
            calibration_version: "scripted-v1".to_owned(),
        })
    }
    async fn update(&self, _labeled: LabeledExample) -> Result<(), MlError> {
        Ok(())
    }
}

fn message(from: &str, subject: &str) -> MessageData {
    MessageData {
        id: Some(MessageId::from("msg_1")),
        client_message_id: "1".to_owned(),
        account_id: AccountId::from("acct_a"),
        folder_id: FolderId::from("folder_inbox"),
        thread_id: None,
        headers: MessageHeaders {
            from: from.to_owned(),
            subject: subject.to_owned(),
            ..MessageHeaders::default()
        },
        body_text: Some("please see attached".to_owned()),
        attachments: vec![],
        remote_content_loaded: false,
    }
}

fn rule(
    id: &str,
    kind: RuleKind,
    field: &str,
    op: Operator,
    value: FieldValue,
    effect: RuleEffect,
) -> EvaluatableRule {
    EvaluatableRule {
        rule_id: RuleId::from(id),
        kind,
        scope: RuleScope::Global,
        band: HierarchyBand::LearnedActive,
        status: RuleStatus::Active,
        version: RuleVersion {
            id: RuleVersionId::from(format!("rv_{id}")),
            version_number: 1,
            condition: Condition::Predicate(Predicate {
                field: field.to_owned(),
                op,
                value,
            }),
            effect,
            risk_level: RiskLevel::Low,
        },
    }
}

fn service(
    classification_rules: Vec<EvaluatableRule>,
    action_rules: Vec<EvaluatableRule>,
    tier2: Arc<dyn Tier2Classifier>,
    provider: Option<Arc<MockProvider>>,
) -> PlanningService {
    let mut cascade = CascadeClassifier::new(
        Arc::new(DeterministicRuleEngine::new(classification_rules)),
        tier2,
    );
    if let Some(provider) = provider {
        cascade = cascade.with_provider(provider);
    }
    let planner = DefaultActionPlanner::new(Arc::new(DeterministicRuleEngine::new(action_rules)));
    PlanningService::new(
        Arc::new(StubFeatureExtractor),
        Arc::new(cascade),
        Arc::new(planner),
        Arc::new(HardPolicyGuard::new()),
    )
}

#[test]
fn classify_end_to_end_through_the_core_using_a_mock_provider() {
    // Ambiguous Tier-2 → escalate to the mock provider, which labels it "newsletter";
    // an active action rule then tags it, and the guard allows the safe tag.
    let provider = MockProvider::returning_json(
        "mock",
        json!({
            "labels": ["newsletter"], "spam_score": 0.2, "phishing_score": 0.0, "priority": "low"
        }),
    );
    let action_rule = rule(
        "rule_news",
        RuleKind::Action,
        "classification.labels",
        Operator::Contains,
        FieldValue::Text("newsletter".to_owned()),
        RuleEffect {
            tag: vec!["newsletter".to_owned()],
            ..RuleEffect::new()
        },
    );
    let service = service(
        vec![],
        vec![action_rule],
        Arc::new(ScriptedTier2::new(&[("ham", 0.55), ("spam", 0.45)])),
        Some(Arc::new(provider)),
    );

    let outcome =
        block_on(service.handle_new_mail(message("news@list.example", "Weekly digest"))).unwrap();

    // The mock provider drove the classification (Tier 3).
    assert_eq!(
        outcome.classification.provenance.tier,
        ClassificationTier::Tier3Llm
    );
    assert_eq!(outcome.classification.labels, vec!["newsletter".to_owned()]);
    // The action rule fired and the guard allowed the safe tag — nothing blocked.
    assert_eq!(outcome.guarded_plan.allowed_actions.len(), 1);
    assert!(outcome.guarded_plan.blocked_actions.is_empty());
    assert!(outcome.guarded_plan.review_required_actions.is_empty());
}

#[test]
fn policy_guard_reviews_an_unsafe_move_from_an_otherwise_matching_rule() {
    // Tier-1 classification rule labels bank mail "financial"; an action rule would move it;
    // the hard policy routes the sensitive move to review (it is never auto-applied).
    let classify_rule = rule(
        "rule_bank",
        RuleKind::Classification,
        "sender_domain",
        Operator::Eq,
        FieldValue::Text("bank.example".to_owned()),
        RuleEffect {
            set_labels: vec!["financial".to_owned()],
            ..RuleEffect::new()
        },
    );
    let move_rule = rule(
        "rule_file_financial",
        RuleKind::Action,
        "classification.labels",
        Operator::Contains,
        FieldValue::Text("financial".to_owned()),
        RuleEffect {
            move_to: Some("Banking".to_owned()),
            ..RuleEffect::new()
        },
    );
    let service = service(
        vec![classify_rule],
        vec![move_rule],
        Arc::new(ScriptedTier2::new(&[("ham", 0.9), ("spam", 0.1)])),
        None,
    );

    let outcome = block_on(service.handle_message(
        message("alerts@bank.example", "Statement ready"),
        TriggerKind::NewMail,
    ))
    .unwrap();

    assert_eq!(
        outcome.classification.provenance.tier,
        ClassificationTier::Tier1Rules
    );
    // The matching move is held for review, not auto-applied; nothing is allowed outright.
    assert!(outcome.guarded_plan.allowed_actions.is_empty());
    assert_eq!(outcome.guarded_plan.review_required_actions.len(), 1);
    assert!(outcome.guarded_plan.blocked_actions.is_empty());
    assert!(outcome
        .guarded_plan
        .policy_checks
        .iter()
        .any(|c| c.policy_id == "financial_security_legal_move_requires_review"));
}

#[test]
fn the_flow_is_fully_functional_with_zero_providers() {
    // Confident Tier-2, no provider configured, no rules: the message classifies and the
    // (empty) plan guards cleanly — MailMate works end-to-end with zero AI providers.
    let service = service(
        vec![],
        vec![],
        Arc::new(ScriptedTier2::new(&[("ham", 0.95), ("spam", 0.05)])),
        None,
    );
    let outcome = block_on(service.handle_new_mail(message("a@b.example", "hello"))).unwrap();
    assert_eq!(
        outcome.classification.provenance.tier,
        ClassificationTier::Tier2Model
    );
    assert!(!outcome.classification.needs_review);
    assert!(outcome.guarded_plan.allowed_actions.is_empty());
    assert!(outcome.guarded_plan.blocked_actions.is_empty());
}
