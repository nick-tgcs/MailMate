//! Integration tests for the three follow-up adapters (DefaultWorkflowEngine,
//! DefaultFollowUpScheduler, DefaultExitDetector) driven over the in-memory fakes. These
//! cover the spec's required follow-up cases: catch-up firing, coalescing to ≤1 draft,
//! staleness → needs-attention, exit-on-reply, won/lost, and the no-auto-send invariant.

use std::sync::Arc;

use futures::executor::block_on;

use mailmate_common::actor::Actor;
use mailmate_common::feedback::FollowUpOutcome;
use mailmate_common::ids::{PipelineItemId, ThreadId, WorkflowDefId};
use mailmate_common::pipeline::{ItemType, NewPipelineItem, PipelineStage};
use mailmate_common::reply::DraftedReply;
use mailmate_common::rules::rule::{RiskLevel, RuleScope};
use mailmate_common::time::Timestamp;
use mailmate_common::workflow::{
    ExitCondition, ExitEvent, FollowUpStep, NewWorkflowDefinition, NewWorkflowInstance,
    ReviewResolution, Staleness, WorkflowAnchor, WorkflowInstanceStatus, WorkflowVersionContent,
};
use mailmate_ports::exit_detector::ExitDetector;
use mailmate_ports::follow_up_scheduler::FollowUpScheduler;
use mailmate_ports::storage::pipeline_items::PipelineItemRepository;
use mailmate_ports::storage::workflows::{WorkflowInstanceRepository, WorkflowRepository};
use mailmate_ports::workflow_engine::WorkflowEngine;
use mailmate_test_support::fakes::{
    FakeAuditRepository, FakeClock, FakeFollowUpFeedbackRepository, FakePipelineItemRepository,
    FakeReplyDrafter, FakeWorkflowInstanceRepository, FakeWorkflowRepository,
};
use mailmate_workflow::{DefaultExitDetector, DefaultFollowUpScheduler, DefaultWorkflowEngine};

fn now() -> Timestamp {
    Timestamp::parse_rfc3339("2026-06-20T00:00:00Z").unwrap()
}

struct Harness {
    items: Arc<FakePipelineItemRepository>,
    workflows: Arc<FakeWorkflowRepository>,
    instances: Arc<FakeWorkflowInstanceRepository>,
    feedback: Arc<FakeFollowUpFeedbackRepository>,
    audit: Arc<FakeAuditRepository>,
    drafter: Arc<FakeReplyDrafter>,
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
            drafter: Arc::new(FakeReplyDrafter::returning(DraftedReply::new(
                "Re: Acme quote",
                "Just checking in on the quote.",
            ))),
            clock: Arc::new(FakeClock::new(now())),
        }
    }

    fn engine(&self) -> DefaultWorkflowEngine {
        DefaultWorkflowEngine::new(
            self.workflows.clone(),
            self.instances.clone(),
            self.items.clone(),
            self.clock.clone(),
        )
    }

    fn scheduler(&self) -> DefaultFollowUpScheduler {
        DefaultFollowUpScheduler::new(
            self.workflows.clone(),
            self.instances.clone(),
            self.items.clone(),
            self.feedback.clone(),
            self.drafter.clone(),
            self.audit.clone(),
        )
    }

    fn exit_detector(&self) -> DefaultExitDetector {
        DefaultExitDetector::new(self.instances.clone(), self.items.clone())
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

    fn seed_workflow(&self, name: &str) -> WorkflowDefId {
        block_on(self.workflows.save_definition_draft(NewWorkflowDefinition {
            stable_name: name.to_owned(),
            scope: RuleScope::Global,
            applies_to_item_type: ItemType::Quote,
            created_by: Actor::User,
            initial_version: cadence(),
        }))
        .unwrap()
    }

    /// Arm an instance directly with an explicit anchor/cursor/next-due (the engine resolves
    /// the anchor from the item's activity; these tests pin it for determinism).
    fn arm_at(
        &self,
        item: &PipelineItemId,
        wf: &WorkflowDefId,
        anchor_at: Timestamp,
        current_step_index: i64,
        next_due_at: Timestamp,
    ) -> mailmate_common::ids::WorkflowInstanceId {
        let def = block_on(self.workflows.get_definition(wf))
            .unwrap()
            .unwrap();
        block_on(self.instances.arm(NewWorkflowInstance {
            pipeline_item_id: item.clone(),
            workflow_id: wf.clone(),
            pinned_def_version_id: def.current_version_id,
            thread_id: ThreadId::from("thread_acme"),
            anchor_at,
            status: WorkflowInstanceStatus::Active,
            current_step_index,
            next_due_at: Some(next_due_at),
        }))
        .unwrap()
    }
}

fn cadence() -> WorkflowVersionContent {
    WorkflowVersionContent {
        title: "Standard quote follow-up".to_owned(),
        description: "3 / 7 / 14".to_owned(),
        anchor: WorkflowAnchor::QuoteSentAt,
        enrollment_condition: None,
        steps: vec![step(0, 3), step(1, 7), step(2, 14)],
        exit_conditions: vec![ExitCondition::ReplyReceived, ExitCondition::Won],
        staleness: Staleness::default(), // coalesce, 14-day horizon
        risk_level: RiskLevel::Medium,
        change_reason: "seed".to_owned(),
        created_by: Actor::User,
    }
}

fn step(idx: i64, offset: i64) -> FollowUpStep {
    FollowUpStep {
        step_index: idx,
        offset_days: offset,
        draft_intent: "gentle_check_in".to_owned(),
        prompt_template_ref: None,
        forbidden_commitments: vec!["prices".to_owned(), "dates".to_owned()],
    }
}

#[test]
fn arm_computes_the_first_due_from_the_anchor() {
    let h = Harness::new();
    let item = h.seed_item();
    let wf = h.seed_workflow("standard");
    let inst_id = block_on(h.engine().arm(item.clone(), wf.clone())).unwrap();

    let inst = block_on(h.instances.get(&inst_id)).unwrap().unwrap();
    assert_eq!(inst.status, WorkflowInstanceStatus::Active);
    assert_eq!(inst.current_step_index, 0);
    // anchor = item.last_activity_at (≈ now); first due = anchor + 3 days.
    let expected = inst.anchor_at.add_days(3);
    assert_eq!(inst.next_due_at, Some(expected));
    assert!(inst.honours_due_invariant());
}

#[test]
fn a_due_step_fires_a_review_required_draft_and_records_feedback() {
    let h = Harness::new();
    let item = h.seed_item();
    let wf = h.seed_workflow("standard");
    let anchor = now().add_days(-5); // step 0 (day3) due 2 days ago, fresh
    let inst_id = h.arm_at(&item, &wf, anchor, 0, anchor.add_days(3));

    let report = block_on(h.scheduler().drain_due(now(), 100)).unwrap();
    assert_eq!(report.fired.len(), 1, "one step fired");
    assert_eq!(report.fired[0].step_index, 0);
    assert!(report.needs_attention.is_empty());

    // The draft is review-required BY CONSTRUCTION — never sent.
    assert!(
        report.fired[0].draft.requires_human_review,
        "follow-up drafts are always review-required"
    );
    assert!(
        h.drafter.requests().len() == 1,
        "the drafter was driven once"
    );

    // The instance is parked in awaiting_review (the frequency cap): next_due cleared.
    let inst = block_on(h.instances.get(&inst_id)).unwrap().unwrap();
    assert_eq!(inst.status, WorkflowInstanceStatus::AwaitingReview);
    assert!(inst.next_due_at.is_none());
    assert!(inst.honours_due_invariant());

    // One followup_feedback row (cadence signal), surfaced_for_review.
    let rows = h.feedback.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, FollowUpOutcome::SurfacedForReview);
    assert_eq!(
        rows[0].draft_id,
        Some(report.fired[0].draft.draft_id.clone())
    );

    // The firing was audited.
    assert!(h
        .audit
        .entries()
        .iter()
        .any(|e| e.event_type == "followup_step_fired"));
}

#[test]
fn the_no_auto_send_invariant_holds_in_the_emitted_report() {
    let h = Harness::new();
    let item = h.seed_item();
    let wf = h.seed_workflow("standard");
    let anchor = now().add_days(-5);
    h.arm_at(&item, &wf, anchor, 0, anchor.add_days(3));
    let report = block_on(h.scheduler().drain_due(now(), 100)).unwrap();
    let json = serde_json::to_string(&report).unwrap();
    assert!(
        !json.contains("send"),
        "no send vocabulary anywhere: {json}"
    );
}

#[test]
fn a_long_absence_coalesces_overdue_steps_to_one_draft() {
    let h = Harness::new();
    let item = h.seed_item();
    let wf = h.seed_workflow("standard");
    // Closed for ~20 days at cursor 1: steps 1 (day7) and 2 (day14) both overdue & fresh.
    let anchor = now().add_days(-20);
    h.arm_at(&item, &wf, anchor, 1, anchor.add_days(7));

    let report = block_on(h.scheduler().drain_due(now(), 100)).unwrap();
    assert_eq!(report.fired.len(), 1, "coalesced to exactly one draft");
    assert_eq!(report.fired[0].step_index, 2, "the latest fresh step fires");
    assert_eq!(
        report.fired[0].coalesced_from,
        vec![1],
        "earlier step skipped"
    );

    let rows = h.feedback.rows();
    assert_eq!(rows[0].coalesced_from, vec![1]);
    assert!(h
        .audit
        .entries()
        .iter()
        .any(|e| e.event_type == "followup_coalesced"));
}

#[test]
fn a_stale_instance_past_the_horizon_needs_attention_with_no_draft() {
    let h = Harness::new();
    let item = h.seed_item();
    let wf = h.seed_workflow("standard");
    // Closed for ~40 days: every step is overdue past the 14-day horizon.
    let anchor = now().add_days(-40);
    let inst_id = h.arm_at(&item, &wf, anchor, 0, anchor.add_days(3));

    let report = block_on(h.scheduler().drain_due(now(), 100)).unwrap();
    assert!(report.fired.is_empty(), "no draft for a stale instance");
    assert_eq!(report.needs_attention.len(), 1);
    assert_eq!(report.needs_attention[0].reason, "stale_past_horizon");
    assert!(
        h.drafter.requests().is_empty(),
        "the drafter is never driven"
    );

    let inst = block_on(h.instances.get(&inst_id)).unwrap().unwrap();
    assert_eq!(inst.status, WorkflowInstanceStatus::NeedsAttention);
    assert!(inst.next_due_at.is_none());
    let rows = h.feedback.rows();
    assert_eq!(rows[0].outcome, FollowUpOutcome::ExpiredNeedsAttention);
}

#[test]
fn a_reply_exits_the_sequence_to_engaged() {
    let h = Harness::new();
    let item = h.seed_item();
    let wf = h.seed_workflow("standard");
    let anchor = now().add_days(-1);
    let inst_id = h.arm_at(&item, &wf, anchor, 0, anchor.add_days(3));

    let exited = block_on(
        h.exit_detector()
            .on_exit_event(item.clone(), ExitEvent::ReplyReceived),
    )
    .unwrap();
    assert_eq!(exited, vec![inst_id.clone()]);
    let inst = block_on(h.instances.get(&inst_id)).unwrap().unwrap();
    assert_eq!(inst.status, WorkflowInstanceStatus::Engaged);
    assert!(inst.next_due_at.is_none());
    let item_row = block_on(h.items.get(&item)).unwrap().unwrap();
    assert_eq!(item_row.stage, PipelineStage::Engaged);
}

#[test]
fn won_completes_the_sequence_and_closes_the_deal() {
    let h = Harness::new();
    let item = h.seed_item();
    let wf = h.seed_workflow("standard");
    let anchor = now().add_days(-1);
    let inst_id = h.arm_at(&item, &wf, anchor, 0, anchor.add_days(3));

    block_on(
        h.exit_detector()
            .on_exit_event(item.clone(), ExitEvent::Won),
    )
    .unwrap();
    let inst = block_on(h.instances.get(&inst_id)).unwrap().unwrap();
    assert_eq!(inst.status, WorkflowInstanceStatus::Completed);
    assert_eq!(
        block_on(h.items.get(&item)).unwrap().unwrap().stage,
        PipelineStage::Won
    );
}

#[test]
fn resolving_a_review_re_arms_the_next_step() {
    let h = Harness::new();
    let item = h.seed_item();
    let wf = h.seed_workflow("standard");
    let anchor = now().add_days(-5);
    let inst_id = h.arm_at(&item, &wf, anchor, 0, anchor.add_days(3));
    // Fire step 0 → awaiting_review.
    block_on(h.scheduler().drain_due(now(), 100)).unwrap();

    block_on(
        h.engine()
            .resolve_review(inst_id.clone(), ReviewResolution::Send, now()),
    )
    .unwrap();
    let inst = block_on(h.instances.get(&inst_id)).unwrap().unwrap();
    assert_eq!(inst.status, WorkflowInstanceStatus::Active, "re-armed");
    assert_eq!(inst.current_step_index, 1, "advanced to the next step");
    assert_eq!(inst.next_due_at, Some(anchor.add_days(7)));
}

#[test]
fn resolving_a_review_on_a_non_awaiting_instance_is_an_invalid_state() {
    let h = Harness::new();
    let item = h.seed_item();
    let wf = h.seed_workflow("standard");
    let anchor = now().add_days(-1);
    let inst_id = h.arm_at(&item, &wf, anchor, 0, anchor.add_days(3));
    let err = block_on(
        h.engine()
            .resolve_review(inst_id, ReviewResolution::Skip, now()),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        mailmate_common::error::WorkflowError::InvalidState(_)
    ));
}

#[test]
fn rescheduling_an_awaiting_review_instance_is_rejected_and_does_not_double_fire() {
    let h = Harness::new();
    let item = h.seed_item();
    let wf = h.seed_workflow("standard");
    let anchor = now().add_days(-5);
    let inst_id = h.arm_at(&item, &wf, anchor, 0, anchor.add_days(3));
    // Fire step 0 → awaiting_review (cursor now points at the fired step).
    block_on(h.scheduler().drain_due(now(), 100)).unwrap();
    assert_eq!(
        block_on(h.instances.get(&inst_id)).unwrap().unwrap().status,
        WorkflowInstanceStatus::AwaitingReview
    );

    // A reschedule on an awaiting_review instance must be rejected (it would otherwise
    // re-arm the cursor at the already-fired step and double-fire on the next drain).
    let err = block_on(
        h.engine()
            .reschedule(inst_id.clone(), now().add_days(2), false),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        mailmate_common::error::WorkflowError::InvalidState(_)
    ));
    // It stays parked in awaiting_review (still the frequency cap), so a second drain fires
    // nothing.
    let after = block_on(h.instances.get(&inst_id)).unwrap().unwrap();
    assert_eq!(after.status, WorkflowInstanceStatus::AwaitingReview);
    assert!(after.next_due_at.is_none());
    let report = block_on(h.scheduler().drain_due(now(), 100)).unwrap();
    assert!(
        report.fired.is_empty(),
        "no second draft for the fired step"
    );
    assert_eq!(
        h.feedback.rows().len(),
        1,
        "exactly one surfaced-for-review row"
    );
}

#[test]
fn detect_conflicts_flags_a_second_workflow_on_the_same_item() {
    let h = Harness::new();
    let item = h.seed_item();
    let wf_a = h.seed_workflow("standard-a");
    let wf_b = h.seed_workflow("standard-b");
    let anchor = now().add_days(-1);
    h.arm_at(&item, &wf_a, anchor, 0, anchor.add_days(3));

    let conflicts = block_on(h.engine().detect_conflicts(&item, &wf_b)).unwrap();
    assert_eq!(conflicts.len(), 1, "the active instance contains the item");
    assert_eq!(conflicts[0].workflow_b_id, wf_b.as_str());
}

// --- the engine's "missing referent" guards -------------------------------------------------
// Every engine entry point resolves its referent from a repository and fails with NotFound when
// it is absent, rather than panicking or arming a dangling instance.

#[test]
fn arming_an_unknown_pipeline_item_is_not_found() {
    let h = Harness::new();
    let wf = h.seed_workflow("standard");
    let err = block_on(h.engine().arm(PipelineItemId::from("missing_item"), wf)).unwrap_err();
    assert!(matches!(err, mailmate_common::error::WorkflowError::NotFound(_)));
}

#[test]
fn arming_with_an_unknown_workflow_definition_is_not_found() {
    let h = Harness::new();
    let item = h.seed_item();
    let err = block_on(h.engine().arm(item, WorkflowDefId::from("missing_wf"))).unwrap_err();
    assert!(matches!(err, mailmate_common::error::WorkflowError::NotFound(_)));
}

#[test]
fn rescheduling_an_unknown_instance_is_not_found() {
    let h = Harness::new();
    let err = block_on(h.engine().reschedule(
        mailmate_common::ids::WorkflowInstanceId::from("missing_inst"),
        now().add_days(1),
        false,
    ))
    .unwrap_err();
    assert!(matches!(err, mailmate_common::error::WorkflowError::NotFound(_)));
}

#[test]
fn resolving_a_review_on_an_unknown_instance_is_not_found() {
    let h = Harness::new();
    let err = block_on(h.engine().resolve_review(
        mailmate_common::ids::WorkflowInstanceId::from("missing_inst"),
        ReviewResolution::Send,
        now(),
    ))
    .unwrap_err();
    assert!(matches!(err, mailmate_common::error::WorkflowError::NotFound(_)));
}
