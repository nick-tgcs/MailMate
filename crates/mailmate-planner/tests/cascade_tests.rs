//! Pipeline-1 cascade gating (architecture.md → Testing Strategy → Integration tests:
//! "a Tier-1/Tier-2-confident message never reaches the LLM task; an ambiguous one
//! escalates"). Deterministic: a real rule engine, scripted Tier-2 scores, and a
//! call-counting provider — no network, no model runtime.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::executor::block_on;
use serde_json::json;

use mailmate_common::ai::{
    ProviderCapabilities, ProviderId, StructuredRequest, StructuredResponse,
};
use mailmate_common::classification::{CascadeThresholds, ClassificationInput, ClassificationTier};
use mailmate_common::error::{AiError, MlError};
use mailmate_common::features::{CalibratedScores, FeatureVector, LabeledExample};
use mailmate_common::ids::{AccountId, FolderId, MessageId, RuleId, RuleVersionId};
use mailmate_common::mail::{MessageData, MessageHeaders};
use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::rule::{
    EvaluatableRule, HierarchyBand, RiskLevel, RuleKind, RuleScope, RuleStatus, RuleVersion,
};
use mailmate_ports::ai_provider::AiProvider;
use mailmate_ports::classification_engine::ClassificationEngine;
use mailmate_ports::tier2_classifier::Tier2Classifier;

use mailmate_planner::CascadeClassifier;
use mailmate_rules::DeterministicRuleEngine;

// --- test doubles -----------------------------------------------------------

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
            contributions: Vec::new(),
        })
    }
    async fn update(&self, _labeled: LabeledExample) -> Result<(), MlError> {
        Ok(())
    }
}

/// An `AiProvider` that counts how many times it is called and returns a fixed verdict.
struct CountingProvider {
    calls: Arc<AtomicUsize>,
    value: serde_json::Value,
}

#[async_trait]
impl AiProvider for CountingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::from("counting")
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            json_schema: true,
            ..ProviderCapabilities::default()
        }
    }
    async fn complete_structured(
        &self,
        _request: StructuredRequest,
    ) -> Result<StructuredResponse, AiError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(StructuredResponse {
            raw_text: self.value.to_string(),
            parsed_json: self.value.clone(),
            schema_validated_by: Some("counting".to_owned()),
        })
    }
}

// --- builders ---------------------------------------------------------------

fn message() -> MessageData {
    MessageData {
        id: Some(MessageId::from("msg_1")),
        client_message_id: "1".to_owned(),
        account_id: AccountId::from("acct_a"),
        folder_id: FolderId::from("folder_inbox"),
        thread_id: None,
        headers: MessageHeaders {
            from: "someone@unknown.example".to_owned(),
            subject: "hello there".to_owned(),
            ..MessageHeaders::default()
        },
        body_text: Some("body".to_owned()),
        attachments: vec![],
        remote_content_loaded: false,
        sender_seen_count: None,
        sender_in_address_book: None,
    }
}

fn input() -> ClassificationInput {
    ClassificationInput::new(message(), FeatureVector::new())
}

fn classification_rule(condition: Condition, effect: RuleEffect) -> EvaluatableRule {
    EvaluatableRule {
        rule_id: RuleId::from("rule_label"),
        kind: RuleKind::Classification,
        scope: RuleScope::Global,
        band: HierarchyBand::HumanHard,
        status: RuleStatus::Active,
        version: RuleVersion {
            id: RuleVersionId::from("rv_label"),
            version_number: 1,
            condition,
            effect,
            risk_level: RiskLevel::Low,
        },
    }
}

fn predicate(field: &str, op: Operator, value: FieldValue) -> Condition {
    Condition::Predicate(Predicate {
        field: field.to_owned(),
        op,
        value,
    })
}

fn counting_provider() -> (Arc<AtomicUsize>, Arc<dyn AiProvider>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = CountingProvider {
        calls: calls.clone(),
        value: json!({
            "labels": ["escalated"], "spam_score": 0.3, "phishing_score": 0.1, "priority": "normal"
        }),
    };
    (calls, Arc::new(provider))
}

// --- tests ------------------------------------------------------------------

#[test]
fn tier1_rule_short_circuits_and_never_calls_the_provider() {
    // An active classification rule fires → accept at Tier 1, no model, no escalation.
    let rule = classification_rule(
        predicate(
            "sender_domain",
            Operator::Eq,
            FieldValue::Text("unknown.example".to_owned()),
        ),
        RuleEffect {
            set_labels: vec!["trusted".to_owned()],
            priority: Some("high".to_owned()),
            ..RuleEffect::new()
        },
    );
    let (calls, provider) = counting_provider();
    let cascade = CascadeClassifier::new(
        Arc::new(DeterministicRuleEngine::new(vec![rule])),
        Arc::new(ScriptedTier2::new(&[("ham", 0.5), ("spam", 0.5)])),
    )
    .with_provider(provider);

    let result = block_on(cascade.classify(input())).unwrap();
    assert_eq!(result.provenance.tier, ClassificationTier::Tier1Rules);
    assert_eq!(result.labels, vec!["trusted".to_owned()]);
    assert_eq!(result.priority.as_str(), "high");
    assert_eq!(
        result.provenance.fired_rules,
        vec![RuleId::from("rule_label")]
    );
    assert!(!result.provenance.escalated);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "Tier-1 must not reach the LLM"
    );
}

#[test]
fn tier2_confident_message_is_accepted_without_escalating() {
    // No rule fires; the local model is confident and the message is clearly not phishing.
    let (calls, provider) = counting_provider();
    let cascade = CascadeClassifier::new(
        Arc::new(DeterministicRuleEngine::new(vec![])),
        Arc::new(ScriptedTier2::new(&[("ham", 0.95), ("spam", 0.05)])),
    )
    .with_provider(provider);

    let result = block_on(cascade.classify(input())).unwrap();
    assert_eq!(result.provenance.tier, ClassificationTier::Tier2Model);
    assert!(!result.needs_review);
    assert_eq!(
        result.provenance.calibration_version.as_deref(),
        Some("scripted-v1")
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "Tier-2-confident must not escalate"
    );
}

#[test]
fn an_ambiguous_message_escalates_to_the_llm() {
    // The local model is unsure (0.55 < the 0.85 band) → escalate to Tier 3.
    let (calls, provider) = counting_provider();
    let cascade = CascadeClassifier::new(
        Arc::new(DeterministicRuleEngine::new(vec![])),
        Arc::new(ScriptedTier2::new(&[("ham", 0.55), ("spam", 0.45)])),
    )
    .with_provider(provider);

    let result = block_on(cascade.classify(input())).unwrap();
    assert_eq!(result.provenance.tier, ClassificationTier::Tier3Llm);
    assert!(result.provenance.escalated);
    assert_eq!(result.labels, vec!["escalated".to_owned()]);
    assert_eq!(result.provenance.provider_id.as_deref(), Some("counting"));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "an ambiguous message escalates exactly once"
    );
}

#[test]
fn a_confident_but_phishy_message_escalates_despite_high_confidence() {
    // Asymmetric safety floor: top-label confidence clears the band, but the phishing score
    // is above the safe floor → escalate readily ("possibly phishing" never auto-clears).
    let (calls, provider) = counting_provider();
    let cascade = CascadeClassifier::new(
        Arc::new(DeterministicRuleEngine::new(vec![])),
        Arc::new(ScriptedTier2::new(&[("ham", 0.9), ("phishing", 0.6)])),
    )
    .with_provider(provider);

    let result = block_on(cascade.classify(input())).unwrap();
    assert_eq!(result.provenance.tier, ClassificationTier::Tier3Llm);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "phishy-but-confident must still escalate"
    );
}

#[test]
fn without_a_provider_a_tier3_needed_message_degrades_to_review() {
    // Ambiguous AND no provider configured → degrade to needs_review, never auto-clear.
    let cascade = CascadeClassifier::new(
        Arc::new(DeterministicRuleEngine::new(vec![])),
        Arc::new(ScriptedTier2::new(&[("ham", 0.55), ("spam", 0.45)])),
    );

    let result = block_on(cascade.classify(input())).unwrap();
    assert!(
        result.needs_review,
        "a Tier-3-needed message with no provider needs review"
    );
    assert_eq!(result.labels, vec!["needs_review".to_owned()]);
    assert!(result.provenance.escalated);
}

#[test]
fn thresholds_are_configurable_and_versioned() {
    // Tightening the band turns a previously-accepted Tier-2 verdict into an escalation.
    let (calls, provider) = counting_provider();
    let cascade = CascadeClassifier::new(
        Arc::new(DeterministicRuleEngine::new(vec![])),
        Arc::new(ScriptedTier2::new(&[("ham", 0.9), ("spam", 0.1)])),
    )
    .with_provider(provider)
    .with_thresholds(CascadeThresholds {
        version: "strict-v9".to_owned(),
        tier2_accept: 0.99,
        phishing_safe_floor: 0.2,
    });

    let result = block_on(cascade.classify(input())).unwrap();
    assert_eq!(result.provenance.tier, ClassificationTier::Tier3Llm);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
