//! Behavioural tests for the Phase-11 follow-up surface of the [`HostRouter`]: the control
//! requests (enroll / update-stage / cancel / reschedule / snooze / review), the
//! catch-up-on-launch drain → `followup_draft_ready` / `followup_needs_attention` frames, and
//! the reply-exit ride on `record_user_action`. Driven over the real follow-up adapters wired
//! to the in-memory fakes — no engine, provider, or backend.

use std::sync::Arc;

use futures::executor::block_on;
use serde_json::{json, Value};

use mailmate_common::actor::Actor;
use mailmate_common::ids::{PipelineItemId, ThreadId, WorkflowDefId, WorkflowInstanceId};
use mailmate_common::pipeline::{ItemType, NewPipelineItem, PipelineStage};
use mailmate_common::protocol::{Frame, ProtocolVersion, ResponseStatus};
use mailmate_common::reply::DraftedReply;
use mailmate_common::rules::rule::{RiskLevel, RuleScope};
use mailmate_common::time::Timestamp;
use mailmate_common::workflow::{
    ExitCondition, FollowUpStep, NewWorkflowDefinition, NewWorkflowInstance, Staleness,
    WorkflowAnchor, WorkflowInstanceStatus, WorkflowVersionContent,
};
use mailmate_core::Ports;
use mailmate_native_host::router::{FollowUpSuite, HostRouter};
use mailmate_ports::storage::pipeline_items::PipelineItemRepository;
use mailmate_ports::storage::workflows::{WorkflowInstanceRepository, WorkflowRepository};
use mailmate_test_support::fakes::{
    FakeActionPlanner, FakeAuditRepository, FakeClassificationEngine, FakeClock,
    FakeFollowUpFeedbackRepository, FakeLearningEngine, FakeMailClient, FakePipelineItemRepository,
    FakePolicyGuard, FakeProposalReview, FakeReplyDrafter, FakeRuleCurator, FakeSecretStore,
    FakeTier2Classifier, FakeTrainingPipeline, FakeTransport, FakeWorkflowInstanceRepository,
    FakeWorkflowRepository, StubFeatureExtractor,
};
use mailmate_workflow::{DefaultExitDetector, DefaultFollowUpScheduler, DefaultWorkflowEngine};

use mailmate_common::classification::{Classification, ClassificationProvenance, Priority};

fn neutral() -> Classification {
    Classification {
        decision_id: mailmate_common::ids::DecisionId::from("dec_seed"),
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

fn base_ports(clock: Arc<FakeClock>, out: Arc<FakeTransport>) -> Ports {
    Ports {
        mail_client: Arc::new(FakeMailClient::new()),
        transport: out,
        clock,
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

fn cadence() -> WorkflowVersionContent {
    WorkflowVersionContent {
        title: "Standard quote follow-up".to_owned(),
        description: "3 / 7 / 14".to_owned(),
        anchor: WorkflowAnchor::QuoteSentAt,
        enrollment_condition: None,
        steps: vec![
            FollowUpStep {
                step_index: 0,
                offset_days: 3,
                draft_intent: "gentle_check_in".to_owned(),
                prompt_template_ref: None,
                forbidden_commitments: vec!["prices".to_owned()],
            },
            FollowUpStep {
                step_index: 1,
                offset_days: 7,
                draft_intent: "value_add".to_owned(),
                prompt_template_ref: None,
                forbidden_commitments: vec![],
            },
        ],
        exit_conditions: vec![ExitCondition::ReplyReceived, ExitCondition::Won],
        staleness: Staleness::default(),
        risk_level: RiskLevel::Medium,
        change_reason: "seed".to_owned(),
        created_by: Actor::User,
    }
}

struct Harness {
    items: Arc<FakePipelineItemRepository>,
    workflows: Arc<FakeWorkflowRepository>,
    instances: Arc<FakeWorkflowInstanceRepository>,
    feedback: Arc<FakeFollowUpFeedbackRepository>,
    audit: Arc<FakeAuditRepository>,
    out: Arc<FakeTransport>,
    clock: Arc<FakeClock>,
}

impl Harness {
    fn new() -> Self {
        Self {
            items: Arc::new(FakePipelineItemRepository::new()),
            workflows: Arc::new(FakeWorkflowRepository::new()),
            instances: Arc::new(FakeWorkflowInstanceRepository::new()),
            feedback: Arc::new(FakeFollowUpFeedbackRepository::new()),
            audit: Arc::new(FakeAuditRepository::new()),
            out: Arc::new(FakeTransport::new()),
            clock: Arc::new(FakeClock::new(Timestamp::now())),
        }
    }

    fn suite(&self, drafter: Arc<FakeReplyDrafter>) -> FollowUpSuite {
        FollowUpSuite {
            pipeline_items: self.items.clone(),
            instances: self.instances.clone(),
            workflow_engine: Arc::new(DefaultWorkflowEngine::new(
                self.workflows.clone(),
                self.instances.clone(),
                self.items.clone(),
                self.clock.clone(),
            )),
            scheduler: Arc::new(DefaultFollowUpScheduler::new(
                self.workflows.clone(),
                self.instances.clone(),
                self.items.clone(),
                self.feedback.clone(),
                drafter,
                self.audit.clone(),
            )),
            exit_detector: Arc::new(DefaultExitDetector::new(
                self.instances.clone(),
                self.items.clone(),
            )),
        }
    }

    fn router(&self) -> HostRouter {
        let drafter = Arc::new(FakeReplyDrafter::returning(DraftedReply::new(
            "Re: Acme quote",
            "Checking in.",
        )));
        HostRouter::from_ports(
            &base_ports(self.clock.clone(), self.out.clone()),
            self.audit.clone(),
            self.out.clone(),
        )
        .with_followups(self.suite(drafter))
    }

    /// A router with no follow-up suite wired (the Phase-10 shape).
    fn router_without_followups(&self) -> HostRouter {
        HostRouter::from_ports(
            &base_ports(self.clock.clone(), self.out.clone()),
            self.audit.clone(),
            self.out.clone(),
        )
    }

    fn seed_workflow(&self) -> WorkflowDefId {
        block_on(self.workflows.save_definition_draft(NewWorkflowDefinition {
            stable_name: "standard-quote-follow-up".to_owned(),
            scope: RuleScope::Global,
            applies_to_item_type: ItemType::Quote,
            created_by: Actor::User,
            initial_version: cadence(),
        }))
        .unwrap()
    }

    fn seed_item(&self) -> PipelineItemId {
        block_on(self.items.insert(NewPipelineItem {
            account_id: "acct_default".to_owned(),
            thread_id: ThreadId::from("thread_acme"),
            anchor_message_id: Some(mailmate_common::ids::MessageId::from("msg_quote")),
            counterparty_email: "buyer@acme.test".to_owned(),
            counterparty_domain: "acme.test".to_owned(),
            title: "Acme quote".to_owned(),
            item_type: ItemType::Quote,
            amount_hint: None,
        }))
        .unwrap()
    }

    /// Arm an active instance with an explicit past anchor/due so a drain fires it now.
    fn arm_overdue(
        &self,
        item: &PipelineItemId,
        wf: &WorkflowDefId,
        cursor: i64,
    ) -> WorkflowInstanceId {
        let def = block_on(self.workflows.get_definition(wf))
            .unwrap()
            .unwrap();
        let anchor = Timestamp::now().add_days(-5);
        block_on(self.instances.arm(NewWorkflowInstance {
            pipeline_item_id: item.clone(),
            workflow_id: wf.clone(),
            pinned_def_version_id: def.current_version_id,
            thread_id: ThreadId::from("thread_acme"),
            anchor_at: anchor,
            status: WorkflowInstanceStatus::Active,
            current_step_index: cursor,
            next_due_at: Some(anchor.add_days(3)),
        }))
        .unwrap()
    }

    fn arm_active_now(&self, item: &PipelineItemId, wf: &WorkflowDefId) -> WorkflowInstanceId {
        let def = block_on(self.workflows.get_definition(wf))
            .unwrap()
            .unwrap();
        let anchor = Timestamp::now();
        block_on(self.instances.arm(NewWorkflowInstance {
            pipeline_item_id: item.clone(),
            workflow_id: wf.clone(),
            pinned_def_version_id: def.current_version_id,
            thread_id: ThreadId::from("thread_acme"),
            anchor_at: anchor,
            status: WorkflowInstanceStatus::Active,
            current_step_index: 0,
            next_due_at: Some(anchor.add_days(3)),
        }))
        .unwrap()
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

fn one_error_code(out: &FakeTransport) -> String {
    let frames = out.sent_frames();
    assert_eq!(frames.len(), 1, "exactly one frame: {frames:?}");
    match &frames[0] {
        Frame::Response {
            status: ResponseStatus::Error,
            error: Some(err),
            ..
        } => err.code.clone(),
        other => panic!("expected an error response, got {other:?}"),
    }
}

#[test]
fn enroll_creates_an_item_and_arms_a_workflow() {
    let h = Harness::new();
    let wf = h.seed_workflow();
    let router = h.router();
    block_on(router.handle(request(
        "enroll_pipeline_item",
        json!({
            "account_id": "acct_default",
            "thread_id": "thread_acme",
            "anchor_thunderbird_message_id": "tb_9",
            "counterparty_email": "buyer@acme.test",
            "counterparty_domain": "acme.test",
            "title": "Acme quote",
            "item_type": "quote",
            "workflow_id": wf.as_str()
        }),
    )))
    .unwrap();

    let payload = one_ok_response(&h.out);
    assert!(payload["pipeline_item_id"]
        .as_str()
        .unwrap()
        .starts_with("pli_"));
    assert!(payload["workflow_instance_id"]
        .as_str()
        .unwrap()
        .starts_with("wfi_"));
    // The item exists and an active instance is armed with a future first due.
    assert_eq!(h.items.all().len(), 1);
    let inst = &h.instances.all()[0];
    assert_eq!(inst.status, WorkflowInstanceStatus::Active);
    assert!(inst.next_due_at.is_some());
    assert!(h
        .audit
        .entries()
        .iter()
        .any(|e| e.event_type == "pipeline_item_enrolled"));
}

#[test]
fn the_drain_emits_a_followup_draft_ready_notification() {
    let h = Harness::new();
    let wf = h.seed_workflow();
    let item = h.seed_item();
    let inst = h.arm_overdue(&item, &wf, 0);
    let router = h.router();

    block_on(router.drain_followups()).unwrap();

    let frames = h.out.sent_frames();
    assert_eq!(frames.len(), 1, "one notification");
    match &frames[0] {
        Frame::Notification { type_, payload, .. } => {
            assert_eq!(type_, "followup_draft_ready");
            assert_eq!(payload["workflow_instance_id"], inst.as_str());
            assert_eq!(payload["draft"]["requires_review"], true);
        }
        other => panic!("expected a notification, got {other:?}"),
    }
    // The instance is parked in awaiting_review (the frequency cap).
    assert_eq!(
        h.instances.all()[0].status,
        WorkflowInstanceStatus::AwaitingReview
    );
    // No send vocabulary in the emitted frame.
    assert!(!serde_json::to_string(&frames[0])
        .unwrap()
        .contains("\"send\""));
}

#[test]
fn update_stage_won_exits_the_sequence_and_closes_the_deal() {
    let h = Harness::new();
    let wf = h.seed_workflow();
    let item = h.seed_item();
    let inst = h.arm_active_now(&item, &wf);
    let router = h.router();

    block_on(router.handle(request(
        "update_pipeline_stage",
        json!({ "pipeline_item_id": item.as_str(), "stage": "won" }),
    )))
    .unwrap();

    let payload = one_ok_response(&h.out);
    assert_eq!(payload["event"], "won");
    assert_eq!(payload["exited"][0], inst.as_str());
    assert_eq!(
        h.instances.all()[0].status,
        WorkflowInstanceStatus::Completed
    );
    assert_eq!(
        block_on(h.items.get(&item)).unwrap().unwrap().stage,
        PipelineStage::Won
    );
    // The exit transition is audited (the mutable-state instance is reconstructable).
    assert!(h
        .audit
        .entries()
        .iter()
        .any(|e| e.event_type == "workflow_exited"));
}

#[test]
fn an_unrecognized_stage_is_an_invalid_stage_error() {
    let h = Harness::new();
    let router = h.router();
    block_on(router.handle(request(
        "update_pipeline_stage",
        json!({ "pipeline_item_id": "pli_1", "stage": "paused" }),
    )))
    .unwrap();
    assert_eq!(one_error_code(&h.out), "invalid_stage");
}

#[test]
fn cancel_sequence_cancels_the_instance() {
    let h = Harness::new();
    let wf = h.seed_workflow();
    let item = h.seed_item();
    h.arm_active_now(&item, &wf);
    let router = h.router();
    block_on(router.handle(request(
        "cancel_sequence",
        json!({ "pipeline_item_id": item.as_str() }),
    )))
    .unwrap();
    assert_eq!(one_ok_response(&h.out)["event"], "cancel");
    assert_eq!(
        h.instances.all()[0].status,
        WorkflowInstanceStatus::Cancelled
    );
}

#[test]
fn snooze_pushes_the_next_due_out_and_marks_snoozed() {
    let h = Harness::new();
    let wf = h.seed_workflow();
    let item = h.seed_item();
    let inst = h.arm_active_now(&item, &wf);
    let router = h.router();
    let later = Timestamp::now().add_days(10);
    block_on(router.handle(request(
        "snooze",
        json!({ "workflow_instance_id": inst.as_str(), "next_due_at": later.to_rfc3339() }),
    )))
    .unwrap();
    let payload = one_ok_response(&h.out);
    assert_eq!(payload["snoozed"], true);
    let stored = &h.instances.all()[0];
    assert_eq!(stored.status, WorkflowInstanceStatus::Snoozed);
    assert_eq!(stored.next_due_at, Some(later));
}

#[test]
fn review_followup_resolves_and_advances_to_the_next_step() {
    let h = Harness::new();
    let wf = h.seed_workflow();
    let item = h.seed_item();
    let inst = h.arm_overdue(&item, &wf, 0);
    let router = h.router();
    // Fire step 0 → awaiting_review (emits one drain notification we don't assert here).
    block_on(router.drain_followups()).unwrap();

    block_on(router.handle(request(
        "review_followup",
        json!({ "workflow_instance_id": inst.as_str(), "resolution": "send" }),
    )))
    .unwrap();
    // The ok response is the LAST frame (after the drain notification).
    let frames = h.out.sent_frames();
    match frames.last().unwrap() {
        Frame::Response {
            payload: Some(p), ..
        } => assert_eq!(p["resolved"], true),
        other => panic!("expected an ok response, got {other:?}"),
    }
    let stored = &h.instances.all()[0];
    assert_eq!(stored.status, WorkflowInstanceStatus::Active, "re-armed");
    assert_eq!(stored.current_step_index, 1);
}

#[test]
fn an_unknown_resolution_is_an_invalid_resolution_error() {
    let h = Harness::new();
    let router = h.router();
    block_on(router.handle(request(
        "review_followup",
        json!({ "workflow_instance_id": "wfi_1", "resolution": "ignore" }),
    )))
    .unwrap();
    assert_eq!(one_error_code(&h.out), "invalid_resolution");
}

#[test]
fn a_reply_on_a_tracked_thread_exits_the_sequence_via_record_user_action() {
    let h = Harness::new();
    let wf = h.seed_workflow();
    let item = h.seed_item();
    let inst = h.arm_active_now(&item, &wf);
    let router = h.router();
    block_on(router.handle(request(
        "record_user_action",
        json!({ "event_type": "reply_received", "thread_id": "thread_acme" }),
    )))
    .unwrap();
    let payload = one_ok_response(&h.out);
    assert_eq!(payload["sink"], "workflow_exit");
    assert!(payload["id"].as_str().unwrap().contains(inst.as_str()));
    assert_eq!(h.instances.all()[0].status, WorkflowInstanceStatus::Engaged);
}

#[test]
fn a_bounce_on_a_tracked_thread_exits_the_sequence_via_record_user_action() {
    let h = Harness::new();
    let wf = h.seed_workflow();
    let item = h.seed_item();
    let inst = h.arm_active_now(&item, &wf);
    let router = h.router();
    // An NDR from mailer-daemon with a delivery-failure subject — a real bounce.
    block_on(router.handle(request(
        "record_user_action",
        json!({
            "event_type": "bounce_received",
            "thread_id": "thread_acme",
            "sender_email": "MAILER-DAEMON@mx.acme.test",
            "subject": "Undelivered Mail Returned to Sender"
        }),
    )))
    .unwrap();
    let payload = one_ok_response(&h.out);
    assert_eq!(payload["sink"], "workflow_exit");
    assert!(payload["id"].as_str().unwrap().contains(inst.as_str()));
    // The sequence is terminal (cancelled) — the unreachable address stops the chase.
    assert_eq!(
        h.instances.all()[0].status,
        WorkflowInstanceStatus::Cancelled
    );
    let exit = h
        .audit
        .entries()
        .into_iter()
        .find(|e| e.event_type == "workflow_exited")
        .expect("a workflow_exited audit row");
    assert_eq!(exit.payload["event"], "bounced");
}

#[test]
fn an_unconfirmed_bounce_does_not_kill_the_sequence() {
    let h = Harness::new();
    let wf = h.seed_workflow();
    let item = h.seed_item();
    h.arm_active_now(&item, &wf);
    let router = h.router();
    // A `bounce_received` whose message does NOT look like an NDR — the host re-confirms and
    // declines to exit, so a mis-tagged or spoofed event can't silently kill a live sequence.
    block_on(router.handle(request(
        "record_user_action",
        json!({
            "event_type": "bounce_received",
            "thread_id": "thread_acme",
            "sender_email": "dana@acme.test",
            "subject": "Re: the proposal — looks good"
        }),
    )))
    .unwrap();
    assert_eq!(one_ok_response(&h.out)["sink"], "bounce_unconfirmed");
    assert_eq!(
        h.instances.all()[0].status,
        WorkflowInstanceStatus::Active,
        "an unconfirmed bounce leaves the sequence running"
    );
    assert!(h
        .audit
        .entries()
        .iter()
        .any(|e| e.event_type == "bounce_unconfirmed"));
}

#[test]
fn followup_requests_without_a_wired_suite_are_rejected() {
    let h = Harness::new();
    let router = h.router_without_followups();
    block_on(router.handle(request(
        "enroll_pipeline_item",
        json!({
            "account_id": "a", "thread_id": "t", "counterparty_email": "x@y.test",
            "workflow_id": "wfd_1"
        }),
    )))
    .unwrap();
    assert_eq!(one_error_code(&h.out), "followups_not_configured");
}

#[test]
fn a_reply_without_a_wired_suite_falls_through_to_audit() {
    let h = Harness::new();
    let router = h.router_without_followups();
    block_on(router.handle(request(
        "record_user_action",
        json!({ "event_type": "reply_received", "thread_id": "thread_acme" }),
    )))
    .unwrap();
    // No follow-up wiring → it is recorded as plain provenance, not a workflow exit.
    assert_eq!(one_ok_response(&h.out)["sink"], "audit");
}

#[test]
fn list_followups_joins_each_deal_to_its_instance_status_and_filters() {
    let h = Harness::new();
    let wf = h.seed_workflow();
    let item = h.seed_item();
    let inst = h.arm_active_now(&item, &wf);
    let router = h.router();

    // Unfiltered: the deal appears, joined to its active instance.
    block_on(router.handle(request("list_followups", json!({})))).unwrap();
    let payload = one_ok_response(&h.out);
    let followups = payload["followups"].as_array().unwrap();
    assert_eq!(followups.len(), 1);
    let deal = &followups[0];
    assert_eq!(deal["pipeline_item_id"], item.as_str());
    assert_eq!(deal["title"], "Acme quote");
    assert_eq!(deal["stage"], "open");
    assert_eq!(deal["status"], "active");
    assert_eq!(deal["workflow_instance_id"], inst.as_str());
    assert_eq!(deal["needs_attention"], false);

    // A `won` filter excludes the open deal.
    let h2 = Harness::new();
    let wf2 = h2.seed_workflow();
    let item2 = h2.seed_item();
    h2.arm_active_now(&item2, &wf2);
    let router2 = h2.router();
    block_on(router2.handle(request("list_followups", json!({ "status_filter": "won" })))).unwrap();
    assert_eq!(
        one_ok_response(&h2.out)["followups"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn list_followups_without_a_wired_suite_is_followups_not_configured() {
    let h = Harness::new();
    let router = h.router_without_followups();
    block_on(router.handle(request("list_followups", json!({})))).unwrap();
    assert_eq!(one_error_code(&h.out), "followups_not_configured");
}
