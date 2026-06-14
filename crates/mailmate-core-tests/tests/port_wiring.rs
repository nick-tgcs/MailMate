//! Contract test: the core's `Ports` bundle composes from the in-memory fakes and
//! routes calls through the boxed `dyn Port` to the concrete adapter — proving the
//! object-safe, cross-crate dependency-injection seam works end to end.

use std::sync::Arc;

use futures::executor::block_on;
use mailmate_common::classification::{Classification, ClassificationProvenance, Priority};
use mailmate_common::ids::DecisionId;
use mailmate_common::ids::MessageId;
use mailmate_common::mail::MailAction;
use mailmate_common::time::Timestamp;
use mailmate_core::Ports;
use mailmate_test_support::fakes::{
    FakeActionPlanner, FakeClassificationEngine, FakeClock, FakeLearningEngine, FakeMailClient,
    FakePolicyGuard, FakeSecretStore, FakeTier2Classifier, FakeTransport, StubFeatureExtractor,
};

fn neutral_classification() -> Classification {
    Classification {
        decision_id: DecisionId::from("dec_seed"),
        labels: vec!["general".to_owned()],
        spam_score: 0.0,
        phishing_score: 0.0,
        priority: Priority::Normal,
        needs_review: false,
        provenance: ClassificationProvenance::tier1(vec![]),
    }
}

#[test]
fn core_ports_compose_from_fakes_and_route_calls() {
    let mail = Arc::new(FakeMailClient::new());
    let ports = Ports {
        mail_client: mail.clone(),
        transport: Arc::new(FakeTransport::new()),
        clock: Arc::new(FakeClock::new(Timestamp::now())),
        secret_store: Arc::new(FakeSecretStore::new()),
        feature_extractor: Arc::new(StubFeatureExtractor),
        tier2: Arc::new(FakeTier2Classifier::new()),
        classification_engine: Arc::new(FakeClassificationEngine::returning(
            neutral_classification(),
        )),
        action_planner: Arc::new(FakeActionPlanner::returning(vec![])),
        policy_guard: Arc::new(FakePolicyGuard::new()),
        learning_engine: Arc::new(FakeLearningEngine::new()),
    };

    // A call made through the boxed port reaches the concrete fake behind it.
    block_on(ports.mail_client.apply(MailAction::MarkRead {
        message_id: MessageId::from("msg_1"),
        read: true,
    }))
    .unwrap();

    assert_eq!(
        mail.applied_actions().len(),
        1,
        "the call routed to the concrete fake"
    );

    // The bundle is cheaply cloneable (use-cases get their own handle).
    let _clone = ports.clone();
}
