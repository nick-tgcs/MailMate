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
        confidence: 0.0,
        salient_signals: Vec::new(),
        safety_findings: Vec::new(),
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
    // Nothing has been applied on a manual classify, so the action is a pending suggestion.
    assert_eq!(payload["suggested_actions"][0]["apply_state"], "suggest");
    // A selected-message classify never auto-applies: the user is in the loop.
    assert!(
        mail.applied_actions().is_empty(),
        "classify applies nothing"
    );
}

#[test]
fn hello_reports_versions_safe_defaults_and_core_capabilities() {
    let out = Arc::new(FakeTransport::new());
    let ports = base_ports();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());

    block_on(router.handle(request(
        "hello",
        json!({ "extension_version": "0.1.0", "protocol_version": "1.0" }),
    )))
    .unwrap();

    let payload = one_ok_response(&out);
    assert_eq!(payload["protocol_version"], "1.0");
    assert!(
        payload["host_version"]
            .as_str()
            .unwrap()
            .starts_with(|c: char| c.is_ascii_digit()),
        "host_version is a real version string"
    );
    // No admin/follow-ups wired → safe local-first defaults and only the core capabilities.
    assert_eq!(payload["drafting_available"], false);
    assert_eq!(payload["retention_level"], "metadata");
    let caps: Vec<&str> = payload["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert!(caps.contains(&"classify_message"));
    assert!(caps.contains(&"record_user_action"));
    assert!(caps.contains(&"explain_decision"));
    // The Activity tab's global stream rides the always-present audit store, so it is a core
    // capability even with no admin surface wired.
    assert!(caps.contains(&"list_recent_activity"));
    assert!(
        !caps.contains(&"followups"),
        "follow-ups are not wired here"
    );
    assert!(!caps.contains(&"get_settings"), "admin is not wired here");
}

#[test]
fn list_recent_activity_returns_newest_first_and_filters_by_family() {
    use mailmate_common::actor::Actor;
    use mailmate_common::audit::AuditEntry;
    use mailmate_ports::storage::AuditRepository;

    let audit = Arc::new(FakeAuditRepository::new());
    // Seed a few entries across families, oldest → newest. The scheduler-authored
    // `followup_step_fired` and the extension's `action_failed` exercise the family arms that
    // group events from outside this router.
    block_on(audit.append(AuditEntry::new("action_applied", Actor::System))).unwrap();
    block_on(audit.append(AuditEntry::new("action_failed", Actor::Extension))).unwrap();
    block_on(audit.append(AuditEntry::new("action_blocked_by_policy", Actor::System))).unwrap();
    block_on(audit.append(AuditEntry::new("followup_step_fired", Actor::System))).unwrap();
    block_on(audit.append(AuditEntry::new("suggestion_dismissed", Actor::User))).unwrap();

    let ports = base_ports();

    // Unfiltered: newest-first, every event.
    let out = Arc::new(FakeTransport::new());
    let router = HostRouter::from_ports(&ports, audit.clone(), out.clone());
    block_on(router.handle(request("list_recent_activity", json!({ "limit": 10 })))).unwrap();
    let payload = one_ok_response(&out);
    let events = payload["events"].as_array().unwrap();
    assert_eq!(events.len(), 5);
    assert_eq!(events[0]["event_type"], "suggestion_dismissed"); // newest first

    // The `applied` family groups both a success and the async `action_failed` failure spelling.
    let out2 = Arc::new(FakeTransport::new());
    let router2 = HostRouter::from_ports(&ports, audit.clone(), out2.clone());
    block_on(router2.handle(request(
        "list_recent_activity",
        json!({ "event_type_filter": "applied" }),
    )))
    .unwrap();
    let p2 = one_ok_response(&out2);
    let applied = p2["events"].as_array().unwrap();
    assert_eq!(applied.len(), 2);
    let applied_types: Vec<&str> = applied
        .iter()
        .map(|e| e["event_type"].as_str().unwrap())
        .collect();
    assert!(applied_types.contains(&"action_applied"));
    assert!(applied_types.contains(&"action_failed"));

    // The `follow_up` family includes the scheduler's per-step lifecycle events.
    let out4 = Arc::new(FakeTransport::new());
    let router4 = HostRouter::from_ports(&ports, audit.clone(), out4.clone());
    block_on(router4.handle(request(
        "list_recent_activity",
        json!({ "event_type_filter": "follow_up" }),
    )))
    .unwrap();
    let p4 = one_ok_response(&out4);
    let followups = p4["events"].as_array().unwrap();
    assert_eq!(followups.len(), 1);
    assert_eq!(followups[0]["event_type"], "followup_step_fired");

    // The "corrected" family captures a dismissal; an empty payload is a valid "all" request.
    let out3 = Arc::new(FakeTransport::new());
    let router3 = HostRouter::from_ports(&ports, audit, out3.clone());
    block_on(router3.handle(request(
        "list_recent_activity",
        json!({ "event_type_filter": "corrected" }),
    )))
    .unwrap();
    let p3 = one_ok_response(&out3);
    assert_eq!(p3["events"].as_array().unwrap().len(), 1);
    assert_eq!(p3["events"][0]["event_type"], "suggestion_dismissed");
}

#[test]
fn review_rule_proposal_applies_the_decision_through_the_review_port() {
    let review = Arc::new(FakeProposalReview::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.proposal_review = review.clone();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());

    // Accept → materializes to the recommended status (the fake synthesizes `accepted`).
    block_on(router.handle(request(
        "review_rule_proposal",
        json!({ "proposal_id": "prop_1", "decision": "accept_for_shadow_mode" }),
    )))
    .unwrap();
    let payload = one_ok_response(&out);
    assert_eq!(payload["reviewed"], true);
    assert_eq!(payload["proposal_id"], "prop_1");
    assert_eq!(payload["resulting_status"], "accepted");
    assert_eq!(review.decisions().len(), 1);

    // Reject → the curator's negative signal is recorded.
    let out2 = Arc::new(FakeTransport::new());
    let router2 =
        HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out2.clone());
    block_on(router2.handle(request(
        "review_rule_proposal",
        json!({ "proposal_id": "prop_2", "decision": "reject", "reason_code": "too_broad" }),
    )))
    .unwrap();
    assert_eq!(one_ok_response(&out2)["resulting_status"], "rejected");
    assert_eq!(review.decisions().len(), 2);
}

#[test]
fn review_rule_proposal_rejects_an_unknown_decision() {
    let out = Arc::new(FakeTransport::new());
    let ports = base_ports();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());

    block_on(router.handle(request(
        "review_rule_proposal",
        json!({ "proposal_id": "prop_1", "decision": "delete_everything" }),
    )))
    .unwrap();

    assert_eq!(error_code(&out), "invalid_decision");
}

#[test]
fn classification_corrected_routes_a_wrong_category_to_classification_feedback() {
    let learning = Arc::new(FakeLearningEngine::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.learning_engine = learning.clone();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());

    block_on(router.handle(request(
        "record_user_action",
        json!({
            "event_type": "classification_corrected",
            "thunderbird_message_id": "tb_42",
            "corrected_label": "newsletters",
            "prior_label": "receipts",
            "user_initiated": true
        }),
    )))
    .unwrap();

    assert_eq!(one_ok_response(&out)["sink"], "classification_feedback");
    match &learning.recorded_feedback()[0] {
        TaskFeedback::Classification(row) => {
            assert_eq!(row.human_label, "newsletters");
            // The prior label rides in as the AI label so the override is recorded honestly.
            assert_eq!(row.ai_label.as_deref(), Some("receipts"));
            assert_eq!(
                row.polarity,
                mailmate_common::feedback::FeedbackPolarity::Negative
            );
        }
        other => panic!("expected a classification row, got {other:?}"),
    }
}

#[test]
fn classification_corrected_without_a_label_is_a_record_failed_error() {
    let out = Arc::new(FakeTransport::new());
    let ports = base_ports();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());
    block_on(router.handle(request(
        "record_user_action",
        json!({ "event_type": "classification_corrected", "thunderbird_message_id": "tb_42" }),
    )))
    .unwrap();
    assert_eq!(error_code(&out), "record_failed");
}

#[test]
fn action_undone_of_a_move_routes_to_filing_feedback_against_the_rule_target() {
    let learning = Arc::new(FakeLearningEngine::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.learning_engine = learning.clone();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());

    // A crystallized rule moved tb_42 into "Promotions"; the user undid it back to "Inbox".
    block_on(router.handle(request(
        "record_user_action",
        json!({
            "event_type": "action_undone",
            "action_kind": "move",
            "thunderbird_message_id": "tb_42",
            "from_folder_id": "Promotions",
            "to_folder_id": "Inbox",
            "rule_id": "R-118",
            "user_initiated": true
        }),
    )))
    .unwrap();

    assert_eq!(one_ok_response(&out)["sink"], "filing_feedback");
    match &learning.recorded_feedback()[0] {
        TaskFeedback::Filing(row) => {
            assert_eq!(row.human_chosen_folder, FolderId::from("Inbox"));
            // The rule's now-undone target is recorded as the diverged AI suggestion...
            assert_eq!(row.ai_suggested_folder, Some(FolderId::from("Promotions")));
            // ...so the row is negative evidence against the rule that fired.
            assert_eq!(
                row.polarity,
                mailmate_common::feedback::FeedbackPolarity::Negative
            );
        }
        other => panic!("expected a filing row, got {other:?}"),
    }
}

#[test]
fn action_undone_of_a_junk_mark_routes_to_not_spam_classification_feedback() {
    let learning = Arc::new(FakeLearningEngine::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.learning_engine = learning.clone();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());

    block_on(router.handle(request(
        "record_user_action",
        json!({
            "event_type": "action_undone",
            "action_kind": "mark_junk",
            "thunderbird_message_id": "tb_42",
            "rule_id": "R-9",
            "user_initiated": true
        }),
    )))
    .unwrap();

    assert_eq!(one_ok_response(&out)["sink"], "classification_feedback");
    match &learning.recorded_feedback()[0] {
        TaskFeedback::Classification(row) => assert_eq!(row.human_label, "ham"),
        other => panic!("expected a classification row, got {other:?}"),
    }
}

#[test]
fn action_undone_of_a_tag_has_no_feedback_table_and_is_audited() {
    let learning = Arc::new(FakeLearningEngine::new());
    let audit = Arc::new(FakeAuditRepository::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.learning_engine = learning.clone();
    let router = HostRouter::from_ports(&ports, audit.clone(), out.clone());

    block_on(router.handle(request(
        "record_user_action",
        json!({
            "event_type": "action_undone",
            "action_kind": "tag",
            "thunderbird_message_id": "tb_42",
            "tag": "needs-review",
            "user_initiated": true
        }),
    )))
    .unwrap();

    assert_eq!(one_ok_response(&out)["sink"], "audit");
    assert!(
        learning.recorded_feedback().is_empty(),
        "a tag has no learned-task feedback table"
    );
    assert!(audit
        .entries()
        .iter()
        .any(|e| e.event_type == "action_undone"));
}

#[test]
fn suggestion_dismissed_is_audited_not_a_fabricated_correction() {
    let learning = Arc::new(FakeLearningEngine::new());
    let audit = Arc::new(FakeAuditRepository::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.learning_engine = learning.clone();
    let router = HostRouter::from_ports(&ports, audit.clone(), out.clone());

    block_on(router.handle(request(
        "record_user_action",
        json!({
            "event_type": "suggestion_dismissed",
            "thunderbird_message_id": "tb_42",
            "action_kind": "move",
            "authored_by": "model",
            "user_initiated": true
        }),
    )))
    .unwrap();

    assert_eq!(one_ok_response(&out)["sink"], "audit");
    assert!(
        learning.recorded_feedback().is_empty(),
        "a dismiss has no chosen label/folder, so it fabricates no feedback row"
    );
    let dismissed = audit
        .entries()
        .iter()
        .find(|e| e.event_type == "suggestion_dismissed")
        .cloned()
        .expect("the dismiss was audited");
    // The ignore-rate provenance the curator can later read is captured on the audit row.
    assert_eq!(dismissed.payload["action_kind"], "move");
    assert_eq!(dismissed.payload["authored_by"], "model");
}

#[test]
fn a_draft_edit_divergence_is_audited_with_the_draft_id() {
    // The edit-divergence learning hook: the user sent a MailMate draft they had edited. There is
    // no chosen label/folder (a draft is not a classification), so it's recorded as audit
    // provenance carrying the draft_id — queryable for an edit-rate, fabricating no feedback row.
    let learning = Arc::new(FakeLearningEngine::new());
    let audit = Arc::new(FakeAuditRepository::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.learning_engine = learning.clone();
    let router = HostRouter::from_ports(&ports, audit.clone(), out.clone());

    block_on(router.handle(request(
        "record_user_action",
        json!({
            "event_type": "draft_diverged",
            "draft_id": "draft_7",
            "thread_id": "thread_1",
            "user_initiated": true
        }),
    )))
    .unwrap();

    assert_eq!(one_ok_response(&out)["sink"], "audit");
    assert!(
        learning.recorded_feedback().is_empty(),
        "a draft edit has no chosen label/folder, so it fabricates no feedback row"
    );
    let diverged = audit
        .entries()
        .iter()
        .find(|e| e.event_type == "draft_diverged")
        .cloned()
        .expect("the divergence was audited");
    assert_eq!(diverged.payload["draft_id"], "draft_7");
    assert_eq!(diverged.actor, mailmate_common::actor::Actor::User);
}

#[test]
fn action_undone_of_a_move_without_a_target_folder_is_a_record_failed_error() {
    let learning = Arc::new(FakeLearningEngine::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.learning_engine = learning.clone();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());
    // A reverted move must carry the folder it was put back to; without it the undo cannot
    // become a filing correction, so it must error rather than fabricate one.
    block_on(router.handle(request(
        "record_user_action",
        json!({
            "event_type": "action_undone",
            "action_kind": "move",
            "thunderbird_message_id": "tb_42",
            "from_folder_id": "Promotions",
            "user_initiated": true
        }),
    )))
    .unwrap();
    assert_eq!(error_code(&out), "record_failed");
    assert!(
        learning.recorded_feedback().is_empty(),
        "no filing feedback is fabricated from a missing reverted-to folder"
    );
}

#[test]
fn new_mail_never_auto_applies_a_require_review_action_in_the_allowed_list() {
    let mail = Arc::new(FakeMailClient::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.mail_client = mail.clone();
    // FakePolicyGuard projects every action into allowed_actions; a RequireReview landing
    // there must still be skipped by apply_allowed — it is a surfaced flag, never a mutation.
    ports.action_planner = Arc::new(FakeActionPlanner::returning(vec![
        ProposedAction::RequireReview {
            target: "msg_tb_tb_42".to_owned(),
        },
    ]));
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());

    block_on(router.handle(request("new_mail", classify_payload("tb_42")))).unwrap();

    // Nothing reached the mail client, and nothing is listed as applied.
    assert!(
        mail.applied_actions().is_empty(),
        "RequireReview is never auto-applied"
    );
    assert!(mail.created_drafts().is_empty());
    match &out.sent_frames()[0] {
        Frame::Notification { payload, .. } => {
            assert_eq!(payload["applied_actions"].as_array().unwrap().len(), 0);
        }
        other => panic!("got {other:?}"),
    }
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
            // Header metadata rides along so the dashboard Review card shows the real identity.
            assert_eq!(payload["headers"]["subject"], "Invoice update");
            assert_eq!(payload["headers"]["from"], "sender@example.test");
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
fn new_mail_applied_move_carries_provenance_and_a_reverses_to_for_undo() {
    // The Phase-1a provenance-spine exit: an auto-applied move must arrive on the wire with
    // `authored_by` (the apply gate — only active rules auto-apply) and a `reverses_to` inverse
    // (a move back to the origin folder) so the per-message panel can offer a REAL Undo.
    let mail = Arc::new(FakeMailClient::new());
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.mail_client = mail.clone();
    ports.action_planner = Arc::new(FakeActionPlanner::returning(vec![ProposedAction::Move {
        message_id: MessageId::from("msg_tb_tb_42"),
        to_folder: FolderId::from("Receipts"),
    }]));
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());

    // classify_payload puts the message in folder "inbox" — the origin a move reverses to.
    block_on(router.handle(request("new_mail", classify_payload("tb_42")))).unwrap();

    let frames = out.sent_frames();
    match &frames[0] {
        Frame::Notification { type_, payload, .. } => {
            assert_eq!(type_, "classification_ready");
            let applied = &payload["applied_actions"][0];
            assert_eq!(applied["kind"], "move");
            assert_eq!(applied["to_folder"], "Receipts");
            assert_eq!(applied["apply_state"], "auto_applied");
            // The apply gate: an auto-applied action is active-rule-authored by construction.
            assert_eq!(applied["authored_by"], "active_rule");
            // A real Undo: reverse the move back to the origin folder.
            assert_eq!(applied["reverses_to"]["kind"], "move");
            assert_eq!(applied["reverses_to"]["to_folder"], "inbox");
        }
        other => panic!("expected a notification, got {other:?}"),
    }
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
fn draft_reply_returns_a_review_required_draft_with_rationale_and_a_clear_guard() {
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    ports.reply_drafter = Arc::new(FakeReplyDrafter::returning(DraftedReply {
        safety_notes: vec!["no prices or dates added".to_owned()],
        rationale: "Polite request for a corrected invoice — no commitments.".to_owned(),
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
    // The model's rationale rides through to the trust surface…
    assert_eq!(
        payload["rationale"],
        "Polite request for a corrected invoice — no commitments."
    );
    // …and the model-free guard ran over the (benign) body: present, and all-clear.
    assert!(
        payload["commitments"]["findings"].as_array().unwrap().is_empty(),
        "a benign body has no commitments: {}",
        payload["commitments"]
    );
}

#[test]
fn draft_reply_runs_the_commitments_guard_over_the_body() {
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    // A body that commits to a date, a price, and a legal position — the guard must surface all
    // three even though the model attached NO safety notes (the guard never trusts the model).
    ports.reply_drafter = Arc::new(FakeReplyDrafter::returning(DraftedReply::new(
        "Re: Order",
        "Yes — I can ship by Friday for $1,200, and I agree to the contract.",
    )));
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());

    block_on(router.handle(request(
        "draft_reply",
        json!({ "subject": "Order", "counterparty": "buyer@acme.test", "excerpt": "when and how much?" }),
    )))
    .unwrap();

    let payload = one_ok_response(&out);
    let findings = payload["commitments"]["findings"].as_array().unwrap();
    let cats: Vec<&str> = findings
        .iter()
        .map(|f| f["category"].as_str().unwrap())
        .collect();
    assert!(cats.contains(&"date"), "got {cats:?}");
    assert!(cats.contains(&"price"), "got {cats:?}");
    assert!(cats.contains(&"legal"), "got {cats:?}");
    // Every finding cites a real, non-empty span.
    for f in findings {
        assert!(!f["text"].as_str().unwrap().is_empty(), "empty span: {f}");
        assert!(f["end"].as_u64().unwrap() > f["start"].as_u64().unwrap());
    }
}

#[test]
fn regenerate_draft_folds_the_steer_into_guidance_and_reruns_the_guard() {
    let out = Arc::new(FakeTransport::new());
    let mut ports = base_ports();
    let drafter = Arc::new(FakeReplyDrafter::returning(DraftedReply::new(
        "Re: Order",
        "Shorter now — let's talk Monday.",
    )));
    ports.reply_drafter = drafter.clone();
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone());

    block_on(router.handle(request(
        "regenerate_draft",
        json!({
            "subject": "Order",
            "counterparty": "buyer@acme.test",
            "excerpt": "can you do better?",
            "user_instruction": "Decline the discount.",
            "adjustments": ["Shorter"],
            "steer": "and propose Monday"
        }),
    )))
    .unwrap();

    let payload = one_ok_response(&out);
    assert_eq!(payload["requires_human_review"], true);
    assert!(payload["draft_id"].as_str().unwrap().starts_with("draft_"));
    // The guard re-ran over the regenerated body ("Monday" is a date).
    let cats: Vec<&str> = payload["commitments"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["category"].as_str().unwrap())
        .collect();
    assert!(cats.contains(&"date"), "got {cats:?}");
    // The chips + free-text steer reached the drafter, after the base instruction.
    let req = &drafter.requests()[0];
    let instruction = req.user_instruction.as_deref().unwrap();
    assert!(instruction.contains("Decline the discount."), "{instruction}");
    assert!(instruction.contains("Make it shorter."), "{instruction}");
    assert!(instruction.contains("and propose Monday"), "{instruction}");
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
