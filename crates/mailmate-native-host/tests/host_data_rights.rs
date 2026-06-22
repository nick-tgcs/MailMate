//! Phase-9 end-to-end probe: delete-my-data / export over the *real* `build_router`
//! composition. It seeds messages and corrections through the public repositories, then drives
//! `forget_message`, `forget_sender`, `reset_learning`, and `export_my_data` as host requests and
//! proves the whole vertical: verb → transactional erasure/export → `data_forgotten` /
//! `data_exported` audit row → response payload, with the rows actually gone from the store.

use std::sync::Arc;

use futures::executor::block_on;
use serde_json::{json, Value};

use mailmate_common::audit::AuditQuery;
use mailmate_common::feedback::{
    ClassificationFeedback, ClassificationFeedbackRow, FeedbackPolarity, PinnedVersions,
};
use mailmate_common::features::FeatureVector;
use mailmate_common::ids::{AccountId, FolderId, MessageId};
use mailmate_common::message::NewMessage;
use mailmate_common::protocol::{Frame, ProtocolVersion, ResponseStatus};
use mailmate_common::retention::RetentionLevel;
use mailmate_common::time::Timestamp;
use mailmate_native_host::config::AppConfig;
use mailmate_native_host::router::HostRouter;
use mailmate_native_host::runtime::build_router;
use mailmate_ports::storage::messages::MessageRepository;
use mailmate_ports::storage::{AuditRepository, FeedbackRepository};
use mailmate_storage::{
    open_and_migrate, SqliteAuditRepository, SqliteBackend, SqliteFeedbackRepository,
    SqliteMessageRepository, StorageConfig,
};
use mailmate_test_support::fakes::FakeTransport;

fn request(type_: &str, payload: Value) -> Frame {
    Frame::Request {
        protocol_version: ProtocolVersion::default(),
        request_id: format!("req_{type_}"),
        type_: type_.to_owned(),
        payload,
    }
}

fn last_ok(out: &FakeTransport) -> Value {
    match out.sent_frames().last().expect("a response frame") {
        Frame::Response {
            status: ResponseStatus::Ok,
            payload: Some(payload),
            ..
        } => payload.clone(),
        other => panic!("expected an ok response, got {other:?}"),
    }
}

fn seed_message(backend: &Arc<SqliteBackend>, id: &str, sender: &str, body: bool) {
    let messages = SqliteMessageRepository::new(backend.clone());
    let domain = sender.split('@').nth(1).unwrap_or("x.test").to_owned();
    block_on(messages.insert(NewMessage {
        id: MessageId::from(id),
        account_id: AccountId::from("acct"),
        folder_id: FolderId::from("inbox"),
        thunderbird_message_id: format!("tb_{id}"),
        rfc_message_id_hash: None,
        thread_id: None,
        sender_email: sender.to_owned(),
        sender_domain: domain,
        subject: format!("subject {id}"),
        received_at: Timestamp::now(),
        body_hash: None,
        body_text: Some(format!("BODY {id}")),
        retention: if body {
            RetentionLevel::Bodies
        } else {
            RetentionLevel::Metadata
        },
        created_at: Timestamp::now(),
    }))
    .unwrap();
}

fn seed_correction(backend: &Arc<SqliteBackend>, id: &str, message_id: &str, label: &str) {
    let feedback: Arc<dyn FeedbackRepository<ClassificationFeedback>> =
        Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    block_on(feedback.append(ClassificationFeedbackRow {
        id: mailmate_common::ids::FeedbackId::from(id),
        message_id: MessageId::from(message_id),
        pinned_versions: PinnedVersions::default(),
        ai_label: None,
        ai_score: None,
        ai_rationale: None,
        human_label: label.to_owned(),
        human_reason_code: None,
        human_reason_text: None,
        salient_features: FeatureVector::new(),
        polarity: FeedbackPolarity::Positive,
        created_at: Timestamp::now(),
    }))
    .unwrap();
}

fn router(backend: &Arc<SqliteBackend>, out: Arc<FakeTransport>) -> HostRouter {
    let dir = std::env::temp_dir().join(format!("mm_data_rights_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    build_router(
        &AppConfig::default(),
        backend,
        out,
        dir.join("secrets.json"),
        None,
        None,
    )
    .unwrap()
}

fn audit_count(backend: &Arc<SqliteBackend>, event_type: &str) -> usize {
    let audit = SqliteAuditRepository::new(backend.clone());
    block_on(audit.query(AuditQuery::default()))
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == event_type)
        .count()
}

#[test]
fn forget_message_erases_the_message_and_leaves_a_data_forgotten_tombstone() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    seed_message(&backend, "msg_1", "a@keep.test", true);
    seed_message(&backend, "msg_2", "b@keep.test", true);
    seed_correction(&backend, "cfb_1", "msg_1", "spam");
    seed_correction(&backend, "cfb_2", "msg_2", "newsletter");
    let messages = SqliteMessageRepository::new(backend.clone());
    let out = Arc::new(FakeTransport::new());
    let router = router(&backend, out.clone());

    block_on(router.handle(request("forget_message", json!({ "message_id": "msg_1" })))).unwrap();
    let report = last_ok(&out);
    assert_eq!(report["scope"], json!("message"));
    assert_eq!(report["removed"]["messages"], json!(1));
    assert_eq!(report["removed"]["classification_feedback"], json!(1));

    // msg_1 is gone; msg_2 untouched.
    assert!(block_on(messages.get(&MessageId::from("msg_1"))).unwrap().is_none());
    assert!(block_on(messages.get(&MessageId::from("msg_2"))).unwrap().is_some());
    // The erasure left exactly one tombstone behind (the message's own audit rows were erased).
    assert_eq!(audit_count(&backend, "data_forgotten"), 1);
}

#[test]
fn forget_sender_erases_that_address_only() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    seed_message(&backend, "s1", "spammer@shared.test", true);
    seed_message(&backend, "f1", "friend@shared.test", true);
    let messages = SqliteMessageRepository::new(backend.clone());
    let out = Arc::new(FakeTransport::new());
    let router = router(&backend, out.clone());

    block_on(router.handle(request(
        "forget_sender",
        json!({ "sender_email": "spammer@shared.test" }),
    )))
    .unwrap();
    assert_eq!(last_ok(&out)["removed"]["messages"], json!(1));
    assert!(block_on(messages.get(&MessageId::from("s1"))).unwrap().is_none());
    assert!(
        block_on(messages.get(&MessageId::from("f1"))).unwrap().is_some(),
        "the same-domain friend survives"
    );
}

#[test]
fn reset_learning_clears_corrections_but_keeps_messages() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    seed_message(&backend, "m", "a@b.test", true);
    seed_correction(&backend, "cfb", "m", "spam");
    let feedback: Arc<dyn FeedbackRepository<ClassificationFeedback>> =
        Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    let messages = SqliteMessageRepository::new(backend.clone());
    let out = Arc::new(FakeTransport::new());
    let router = router(&backend, out.clone());

    block_on(router.handle(request("reset_learning", json!({})))).unwrap();
    assert_eq!(last_ok(&out)["scope"], json!("learning"));
    assert_eq!(last_ok(&out)["removed"]["classification_feedback"], json!(1));

    // The correction corpus is empty, but the message is kept.
    assert!(block_on(
        feedback.query(mailmate_common::feedback::ClassificationFeedbackQuery::default())
    )
    .unwrap()
    .is_empty());
    assert!(block_on(messages.get(&MessageId::from("m"))).unwrap().is_some());
    assert_eq!(audit_count(&backend, "data_forgotten"), 1);
}

#[test]
fn export_my_data_returns_metadata_and_only_retained_bodies() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    seed_message(&backend, "kept", "a@b.test", true); // retained body
    seed_message(&backend, "meta", "c@d.test", false); // metadata-only
    seed_correction(&backend, "cfb", "kept", "spam");
    let out = Arc::new(FakeTransport::new());
    let router = router(&backend, out.clone());

    block_on(router.handle(request("export_my_data", json!({})))).unwrap();
    let export = last_ok(&out);
    let msgs = export["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    let kept = msgs.iter().find(|m| m["id"] == json!("kept")).unwrap();
    assert_eq!(kept["body_text"], json!("BODY kept"));
    let meta = msgs.iter().find(|m| m["id"] == json!("meta")).unwrap();
    assert_eq!(meta["body_retained"], json!(false));
    assert!(meta.get("body_text").is_none(), "a metadata-only message exports no body");
    assert_eq!(export["classification_feedback"].as_array().unwrap().len(), 1);
    assert_eq!(audit_count(&backend, "data_exported"), 1);
}

#[test]
fn forget_message_without_a_message_id_is_an_invalid_payload() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router(&backend, out.clone());

    block_on(router.handle(request("forget_message", json!({})))).unwrap();
    match out.sent_frames().last().unwrap() {
        Frame::Response { status: ResponseStatus::Error, error: Some(err), .. } => {
            assert_eq!(err.code, "invalid_payload");
        }
        other => panic!("expected an error response, got {other:?}"),
    }
}
