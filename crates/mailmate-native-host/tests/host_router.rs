//! Behavioural tests for the [`HostRouter`]: the documented protocol flows driven over the
//! in-memory fakes. Each flow asserts the *exact* frames the extension would consume and the
//! side effects (applied mail actions, captured feedback, audit rows) the host produced — so
//! the wire contract and the routing are pinned without a real engine, provider, or backend.

use std::io::Cursor;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use mailmate_common::action::ProposedAction;
use mailmate_common::classification::{
    Classification, ClassificationInput, ClassificationProvenance, Priority,
};
use mailmate_common::error::ClassificationError;
use mailmate_common::feedback::TaskFeedback;
use mailmate_common::ids::{DecisionId, FolderId, MessageId};
use mailmate_common::protocol::{Frame, ProtocolVersion, ResponseStatus};
use mailmate_common::reply::DraftedReply;
use mailmate_common::time::Timestamp;
use mailmate_core::Ports;
use mailmate_native_host::native_stdio::write_frame;
use mailmate_native_host::router::HostRouter;
use mailmate_ports::classification_engine::ClassificationEngine;
use mailmate_test_support::fakes::{
    FakeActionPlanner, FakeAuditRepository, FakeClassificationEngine, FakeClock,
    FakeLearningEngine, FakeMailClient, FakePolicyGuard, FakeProposalReview, FakeReplyDrafter,
    FakeRuleCurator, FakeSecretStore, FakeTier2Classifier, FakeTrainingPipeline, FakeTransport,
    StubFeatureExtractor,
};

use futures::executor::block_on;

/// A classification engine that always fails (to exercise the error branches).
struct FailingClassificationEngine;

#[async_trait]
impl ClassificationEngine for FailingClassificationEngine {
    async fn classify(
        &self,
        _input: ClassificationInput,
    ) -> Result<Classification, ClassificationError> {
        Err(ClassificationError::Provider("provider down".to_owned()))
    }
}

/// A mail client whose `apply` always fails (to exercise the apply-failure audit branch).
struct FailingMailClient;

#[async_trait]
impl mailmate_ports::mail_client::MailClient for FailingMailClient {
    async fn apply(
        &self,
        _action: mailmate_common::mail::MailAction,
    ) -> Result<(), mailmate_common::error::MailError> {
        Err(mailmate_common::error::MailError::Adapter(
            "apply failed".to_owned(),
        ))
    }
    async fn create_draft(
        &self,
        _spec: mailmate_common::mail::DraftSpec,
    ) -> Result<mailmate_common::ids::DraftId, mailmate_common::error::MailError> {
        Err(mailmate_common::error::MailError::Adapter(
            "draft failed".to_owned(),
        ))
    }
    async fn fetch(
        &self,
        id: MessageId,
        _scope: mailmate_common::mail::FetchScope,
    ) -> Result<mailmate_common::mail::MessageData, mailmate_common::error::MailError> {
        Err(mailmate_common::error::MailError::NotFound(id))
    }
    fn events(&self) -> mailmate_common::stream::EventStream<mailmate_common::mail::MailEvent> {
        Box::pin(futures::stream::empty())
    }
}

fn neutral() -> Classification {
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

/// A `Ports` bundle of all-default fakes; tests overwrite the fields they vary.
fn base_ports() -> Ports {
    Ports {
        mail_client: Arc::new(FakeMailClient::new()),
        transport: Arc::new(FakeTransport::new()),
        clock: Arc::new(FakeClock::new(Timestamp::now())),
        secret_store: Arc::new(FakeSecretStore::new()),
        feature_extractor: Arc::new(StubFeatureExtractor),
        tier2: Arc::new(FakeTier2Classifier::new()),
        classification_engine: Arc::new(FakeClassificationEngine::returning(neutral())),
        action_planner: Arc::new(FakeActionPlanner::returning(vec![])),
        policy_guard: Arc::new(FakePolicyGuard::new()),
        learning_engine: Arc::new(FakeLearningEngine::new()),
        rule_curator: Arc::new(FakeRuleCurator::new()),
        proposal_review: Arc::new(FakeProposalReview::new()),
        reply_drafter: Arc::new(FakeReplyDrafter::new()),
        training_pipeline: Arc::new(FakeTrainingPipeline::new()),
    }
}

fn request(type_: &str, payload: Value) -> Frame {
    Frame::Request {
        protocol_version: ProtocolVersion::default(),
        request_id: format!("req_{type_}"),
        type_: type_.to_owned(),
        payload,
    }
}

fn classify_payload(tb_id: &str) -> Value {
    json!({
        "thunderbird_message_id": tb_id,
        "account_id": "acct_default",
        "folder_id": "inbox",
        "headers": { "from": "sender@example.test", "subject": "Invoice update" },
        "body_text": "please review",
        "body_retention_allowed": false
    })
}

fn tag_action() -> ProposedAction {
    ProposedAction::Tag {
        message_id: MessageId::from("msg_tb_tb_42"),
        tag: "needs-review".to_owned(),
    }
}

/// The single response frame, asserting it is one and is `ok`.
fn one_ok_response(out: &FakeTransport) -> Value {
    let frames = out.sent_frames();
    assert_eq!(frames.len(), 1, "exactly one frame: {frames:?}");
    match &frames[0] {
        Frame::Response {
            status: ResponseStatus::Ok,
            payload: Some(payload),
            ..
        } => payload.clone(),
        other => panic!("expected an ok response, got {other:?}"),
    }
}

#[test]
fn classify_message_returns_a_guarded_plan_and_applies_nothing() {
    let mail = Arc::new(FakeMailClient::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.mail_client = mail.clone();
    ports.action_planner = Arc::new(FakeActionPlanner::returning(vec![tag_action()]));
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());

    block_on(router.handle(request("classify_message", classify_payload("tb_42")))).unwrap();

    let payload = one_ok_response(&out);
    assert_eq!(payload["thunderbird_message_id"], "tb_42");
    assert_eq!(payload["suggested_actions"][0]["kind"], "tag");
    assert_eq!(payload["suggested_actions"][0]["policy_outcome"], "allowed");
    // A selected-message classify never auto-applies: the user is in the loop.
    assert!(
        mail.applied_actions().is_empty(),
        "classify applies nothing"
    );
}

#[test]
fn new_mail_applies_allowed_actions_and_pushes_classification_ready() {
    let mail = Arc::new(FakeMailClient::new());
    let audit = Arc::new(FakeAuditRepository::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.mail_client = mail.clone();
    ports.action_planner = Arc::new(FakeActionPlanner::returning(vec![tag_action()]));
    let router = HostRouter::from_ports(&ports, audit.clone(), out.clone());

    block_on(router.handle(request("new_mail", classify_payload("tb_42")))).unwrap();

    // The host applied the allowed tag through the mail client.
    assert_eq!(
        mail.applied_actions().len(),
        1,
        "host applied the allowed action"
    );
    // It pushed a classification_ready notification listing the applied action.
    let frames = out.sent_frames();
    assert_eq!(frames.len(), 1);
    match &frames[0] {
        Frame::Notification { type_, payload, .. } => {
            assert_eq!(type_, "classification_ready");
            assert_eq!(payload["applied_actions"][0]["kind"], "tag");
        }
        other => panic!("expected a notification, got {other:?}"),
    }
    // The apply was audited.
    assert!(audit
        .entries()
        .iter()
        .any(|e| e.event_type == "action_applied"));
}

#[test]
fn new_mail_blocks_a_prohibited_action_and_never_applies_it() {
    let mail = Arc::new(FakeMailClient::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.mail_client = mail.clone();
    ports.action_planner = Arc::new(FakeActionPlanner::returning(vec![ProposedAction::Delete {
        message_id: MessageId::from("msg_tb_tb_42"),
    }]));
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());

    block_on(router.handle(request("new_mail", classify_payload("tb_42")))).unwrap();

    assert!(
        mail.applied_actions().is_empty(),
        "a blocked action is never applied"
    );
    match &out.sent_frames()[0] {
        Frame::Notification { payload, .. } => {
            assert_eq!(payload["applied_actions"].as_array().unwrap().len(), 0);
            assert_eq!(payload["blocked_actions"][0]["action"]["kind"], "delete");
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn draft_reply_returns_a_review_required_draft() {
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.reply_drafter = Arc::new(FakeReplyDrafter::returning(DraftedReply {
        safety_notes: vec!["no prices or dates added".to_owned()],
        ..DraftedReply::new("Re: Invoice update", "Hi,\n\nCould you re-send?")
    }));
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());

    block_on(router.handle(request(
        "draft_reply",
        json!({
            "thread_id": "thread_1",
            "message_ids": ["tb_1", "tb_2"],
            "subject": "Invoice update",
            "counterparty": "sender@example.test",
            "excerpt": "the invoice was wrong",
            "user_instruction": "Politely ask for a corrected invoice.",
            "forbidden_commitments": ["dates", "prices"]
        }),
    )))
    .unwrap();

    let payload = one_ok_response(&out);
    assert_eq!(payload["requires_human_review"], true);
    assert!(payload["draft_id"].as_str().unwrap().starts_with("draft_"));
    assert_eq!(payload["subject"], "Re: Invoice update");
    assert_eq!(payload["safety_notes"][0], "no prices or dates added");
}

#[test]
fn record_user_action_routes_a_user_move_to_filing_feedback() {
    let learning = Arc::new(FakeLearningEngine::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.learning_engine = learning.clone();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());

    block_on(router.handle(request(
        "record_user_action",
        json!({
            "event_type": "message_moved",
            "thunderbird_message_id": "tb_42",
            "from_folder_id": "inbox",
            "to_folder_id": "Receipts/Software",
            "user_initiated": true,
            "sender_domain": "github.com"
        }),
    )))
    .unwrap();

    assert_eq!(one_ok_response(&out)["sink"], "filing_feedback");
    match &learning.recorded_feedback()[0] {
        TaskFeedback::Filing(row) => {
            assert_eq!(row.human_chosen_folder, FolderId::from("Receipts/Software"));
            assert_eq!(row.sender_domain.as_deref(), Some("github.com"));
        }
        other => panic!("expected a filing row, got {other:?}"),
    }
}

#[test]
fn record_user_action_routes_junk_to_classification_feedback() {
    let learning = Arc::new(FakeLearningEngine::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.learning_engine = learning.clone();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());

    block_on(router.handle(request(
        "record_user_action",
        json!({ "event_type": "junk_changed", "thunderbird_message_id": "tb_42", "junk": true }),
    )))
    .unwrap();

    assert_eq!(one_ok_response(&out)["sink"], "classification_feedback");
    match &learning.recorded_feedback()[0] {
        TaskFeedback::Classification(row) => assert_eq!(row.human_label, "spam"),
        other => panic!("expected a classification row, got {other:?}"),
    }
}

#[test]
fn record_user_action_routes_provenance_and_execution_results_to_audit() {
    let learning = Arc::new(FakeLearningEngine::new());
    let audit = Arc::new(FakeAuditRepository::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.learning_engine = learning.clone();
    let router = HostRouter::from_ports(&ports, audit.clone(), out.clone());

    // A read toggle is pure provenance.
    block_on(router.handle(request(
        "record_user_action",
        json!({ "event_type": "read_changed", "thunderbird_message_id": "tb_42", "read": true }),
    )))
    .unwrap();
    // An execution result the extension reports back.
    block_on(router.handle(request(
        "record_user_action",
        json!({ "event_type": "action_applied", "thunderbird_message_id": "tb_42", "result": "ok" }),
    )))
    .unwrap();

    let frames = out.sent_frames();
    assert_eq!(frames.len(), 2);
    // Neither was routed to a feedback table.
    assert!(
        learning.recorded_feedback().is_empty(),
        "provenance is not a correction"
    );
    let entries = audit.entries();
    assert!(entries.iter().any(|e| e.event_type == "read_changed"));
    let applied = entries
        .iter()
        .find(|e| e.event_type == "action_applied")
        .unwrap();
    assert_eq!(applied.actor, mailmate_common::actor::Actor::Extension);
}

#[test]
fn a_bad_protocol_version_is_a_structured_error() {
    let out = Arc::new(FakeTransport::new());
    let ports = base_ports();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());
    let frame = Frame::Request {
        protocol_version: ProtocolVersion("9.9".to_owned()),
        request_id: "req_x".to_owned(),
        type_: "ping".to_owned(),
        payload: json!({}),
    };
    block_on(router.handle(frame)).unwrap();
    assert_eq!(error_code(&out), "unsupported_protocol_version");
}

#[test]
fn an_unknown_request_type_is_a_structured_error() {
    let out = Arc::new(FakeTransport::new());
    let ports = base_ports();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());
    block_on(router.handle(request("not_a_real_type", json!({})))).unwrap();
    assert_eq!(error_code(&out), "unknown_request_type");
}

#[test]
fn a_response_frame_inbound_is_unexpected_kind() {
    let out = Arc::new(FakeTransport::new());
    let ports = base_ports();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());
    let inbound = Frame::Response {
        protocol_version: ProtocolVersion::default(),
        request_id: "r".to_owned(),
        status: ResponseStatus::Ok,
        payload: Some(json!({})),
        error: None,
    };
    block_on(router.handle(inbound)).unwrap();
    assert_eq!(error_code(&out), "unexpected_kind");
}

#[test]
fn a_malformed_classify_payload_is_an_invalid_payload_error() {
    let out = Arc::new(FakeTransport::new());
    let ports = base_ports();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());
    // Missing the required thunderbird_message_id.
    block_on(router.handle(request("classify_message", json!({ "account_id": "a" })))).unwrap();
    assert_eq!(error_code(&out), "invalid_payload");
}

#[test]
fn a_classification_failure_becomes_an_error_response() {
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.classification_engine = Arc::new(FailingClassificationEngine);
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());
    block_on(router.handle(request("classify_message", classify_payload("tb_42")))).unwrap();
    assert_eq!(error_code(&out), "classification_failed");
}

#[test]
fn a_new_mail_classification_failure_is_audited_and_drops_silently() {
    let audit = Arc::new(FakeAuditRepository::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.classification_engine = Arc::new(FailingClassificationEngine);
    let router = HostRouter::from_ports(&ports, audit.clone(), out.clone());
    block_on(router.handle(request("new_mail", classify_payload("tb_42")))).unwrap();
    // No frame is pushed (a background failure has no request to answer), but it is audited.
    assert!(out.sent_frames().is_empty());
    assert!(audit
        .entries()
        .iter()
        .any(|e| e.event_type == "classification_failed"));
}

#[test]
fn a_new_mail_with_a_malformed_payload_is_audited_and_dropped() {
    let audit = Arc::new(FakeAuditRepository::new());
    let out = Arc::new(FakeTransport::new());
    let ports = base_ports();
    let router = HostRouter::from_ports(&ports, audit.clone(), out.clone());
    block_on(router.handle(request("new_mail", json!({ "nonsense": true })))).unwrap();
    assert!(out.sent_frames().is_empty());
    assert!(audit
        .entries()
        .iter()
        .any(|e| e.event_type == "new_mail_rejected"));
}

#[test]
fn a_draft_failure_becomes_an_error_response() {
    let out = Arc::new(FakeTransport::new());
    // base_ports uses FakeReplyDrafter::new() which errors (no provider configured).
    let ports = base_ports();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());
    block_on(router.handle(request(
        "draft_reply",
        json!({ "subject": "x", "counterparty": "c", "excerpt": "e" }),
    )))
    .unwrap();
    assert_eq!(error_code(&out), "draft_failed");
}

#[test]
fn a_record_with_a_missing_required_field_is_a_record_failed_error() {
    let out = Arc::new(FakeTransport::new());
    let ports = base_ports();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());
    // junk_changed without the message id cannot become a correction.
    block_on(router.handle(request(
        "record_user_action",
        json!({ "event_type": "junk_changed", "junk": true }),
    )))
    .unwrap();
    assert_eq!(error_code(&out), "record_failed");
}

#[test]
fn ping_is_served_by_the_router() {
    let out = Arc::new(FakeTransport::new());
    let ports = base_ports();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());
    block_on(router.handle(request("ping", json!({ "nonce": "abc" })))).unwrap();
    assert_eq!(one_ok_response(&out)["echo"], "abc");
}

#[test]
fn read_message_is_an_alias_for_classify_message() {
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.action_planner = Arc::new(FakeActionPlanner::returning(vec![tag_action()]));
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());
    block_on(router.handle(request("read_message", classify_payload("tb_42")))).unwrap();
    assert_eq!(one_ok_response(&out)["suggested_actions"][0]["kind"], "tag");
}

#[test]
fn a_notification_frame_inbound_is_unexpected_kind() {
    let out = Arc::new(FakeTransport::new());
    let ports = base_ports();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());
    let inbound = Frame::Notification {
        protocol_version: ProtocolVersion::default(),
        notification_id: "ntf_1".to_owned(),
        type_: "classification_ready".to_owned(),
        payload: json!({}),
    };
    block_on(router.handle(inbound)).unwrap();
    assert_eq!(error_code(&out), "unexpected_kind");
}

#[test]
fn new_mail_applies_a_create_draft_action_through_the_mail_client() {
    let mail = Arc::new(FakeMailClient::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.mail_client = mail.clone();
    ports.action_planner = Arc::new(FakeActionPlanner::returning(vec![
        ProposedAction::CreateDraft {
            draft: mailmate_common::mail::DraftSpec {
                subject: "Re: Hi".to_owned(),
                body: "drafted".to_owned(),
                ..mailmate_common::mail::DraftSpec::default()
            },
        },
    ]));
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());
    block_on(router.handle(request("new_mail", classify_payload("tb_42")))).unwrap();
    assert_eq!(mail.created_drafts().len(), 1, "the host created the draft");
}

#[test]
fn an_apply_failure_is_audited_and_not_listed_as_applied() {
    let audit = Arc::new(FakeAuditRepository::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.mail_client = Arc::new(FailingMailClient);
    ports.action_planner = Arc::new(FakeActionPlanner::returning(vec![tag_action()]));
    let router = HostRouter::from_ports(&ports, audit.clone(), out.clone());
    block_on(router.handle(request("new_mail", classify_payload("tb_42")))).unwrap();

    // The failed apply is audited, and the notification lists no applied action.
    assert!(audit
        .entries()
        .iter()
        .any(|e| e.event_type == "action_apply_failed"));
    match &out.sent_frames()[0] {
        Frame::Notification { payload, .. } => {
            assert_eq!(payload["applied_actions"].as_array().unwrap().len(), 0);
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn a_host_initiated_move_is_provenance_not_a_correction() {
    let learning = Arc::new(FakeLearningEngine::new());
    let audit = Arc::new(FakeAuditRepository::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.learning_engine = learning.clone();
    let router = HostRouter::from_ports(&ports, audit.clone(), out.clone());
    // user_initiated:false → MailMate moved it → audit, never filing_feedback.
    block_on(router.handle(request(
        "record_user_action",
        json!({
            "event_type": "message_moved",
            "thunderbird_message_id": "tb_42",
            "to_folder_id": "Receipts",
            "user_initiated": false
        }),
    )))
    .unwrap();
    assert_eq!(one_ok_response(&out)["sink"], "audit");
    assert!(learning.recorded_feedback().is_empty());
    // A host-initiated move is attributed to the System, never the human user.
    let moved = audit
        .entries()
        .iter()
        .find(|e| e.event_type == "message_moved")
        .cloned()
        .expect("the move was audited");
    assert_eq!(moved.actor, mailmate_common::actor::Actor::System);
}

#[test]
fn junk_changed_without_a_junk_field_is_a_record_failed_error_not_a_fabricated_spam_label() {
    let learning = Arc::new(FakeLearningEngine::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.learning_engine = learning.clone();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());
    // The junk discriminator is absent: it must error, not silently become MarkSpam.
    block_on(router.handle(request(
        "record_user_action",
        json!({ "event_type": "junk_changed", "thunderbird_message_id": "tb_42" }),
    )))
    .unwrap();
    assert_eq!(error_code(&out), "record_failed");
    assert!(
        learning.recorded_feedback().is_empty(),
        "no spam label is fabricated from a missing junk state"
    );
}

#[test]
fn serve_blocking_answers_a_sequence_then_handles_malformed_then_eof() {
    let out = Arc::new(FakeTransport::new());
    let ports = base_ports();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());

    // [valid ping][malformed frame][valid ping] on one reader.
    let mut input = Vec::new();
    write_frame(&mut input, &request("ping", json!({ "nonce": 1 }))).unwrap();
    let bad = b"{not json";
    input.extend_from_slice(&(bad.len() as u32).to_ne_bytes());
    input.extend_from_slice(bad);
    write_frame(&mut input, &request("ping", json!({ "nonce": 2 }))).unwrap();

    let mut reader = Cursor::new(input);
    router.serve_blocking(&mut reader).unwrap();

    let frames = out.sent_frames();
    assert_eq!(frames.len(), 3, "two pongs and one malformed error");
    assert!(matches!(
        &frames[0],
        Frame::Response {
            status: ResponseStatus::Ok,
            ..
        }
    ));
    match &frames[1] {
        Frame::Response {
            status: ResponseStatus::Error,
            error: Some(e),
            ..
        } => {
            assert_eq!(e.code, "malformed_frame");
        }
        other => panic!("expected a malformed_frame error, got {other:?}"),
    }
    assert!(matches!(
        &frames[2],
        Frame::Response {
            status: ResponseStatus::Ok,
            ..
        }
    ));
}

/// Decode the single error frame's code.
fn error_code(out: &FakeTransport) -> String {
    let frames = out.sent_frames();
    assert_eq!(frames.len(), 1, "exactly one frame: {frames:?}");
    match &frames[0] {
        Frame::Response {
            status: ResponseStatus::Error,
            error: Some(e),
            ..
        } => e.code.clone(),
        other => panic!("expected an error response, got {other:?}"),
    }
}
