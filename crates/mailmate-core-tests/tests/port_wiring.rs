//! Contract test: the core's `Ports` bundle composes from the in-memory fakes and
//! routes calls through the boxed `dyn Port` to the concrete adapter — proving the
//! object-safe, cross-crate dependency-injection seam works end to end.

use std::sync::Arc;

use futures::executor::block_on;
use mailmate_common::ids::MessageId;
use mailmate_common::mail::MailAction;
use mailmate_common::time::Timestamp;
use mailmate_core::Ports;
use mailmate_test_support::fakes::{
    FakeClock, FakeMailClient, FakeSecretStore, FakeTier2Classifier, FakeTransport,
    StubFeatureExtractor,
};

#[test]
fn core_ports_compose_from_fakes_and_route_calls() {
    let mail = Arc::new(FakeMailClient::new());
    let ports = Ports::new(
        mail.clone(),
        Arc::new(FakeTransport::new()),
        Arc::new(FakeClock::new(Timestamp::now())),
        Arc::new(FakeSecretStore::new()),
        Arc::new(StubFeatureExtractor),
        Arc::new(FakeTier2Classifier::new()),
    );

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
