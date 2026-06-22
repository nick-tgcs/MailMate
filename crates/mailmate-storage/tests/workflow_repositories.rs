//! Integration tests for the Phase-11 follow-up repositories — pipeline items, workflow
//! definitions (+ immutable versions), the mutable-state instances (the durable temporal
//! trigger), follow-up feedback, conflicts, and shadow outcomes — driven through the public
//! adapters over a migrated in-memory backend.

use std::sync::Arc;

use futures::executor::block_on;

use mailmate_common::actor::Actor;
use mailmate_common::error::StorageError;
use mailmate_common::feedback::{
    FeedbackPolarity, FollowUpFeedback, FollowUpFeedbackQuery, FollowUpFeedbackRow,
    FollowUpOutcome, PinnedVersions,
};
use mailmate_common::ids::{
    DraftId, MessageId, PipelineItemId, ThreadId, WorkflowConflictId, WorkflowDefId,
    WorkflowDefVersionId, WorkflowInstanceId, WorkflowShadowOutcomeId,
};
use mailmate_common::pipeline::{ItemType, NewPipelineItem, PipelineItemQuery, PipelineStage};
use mailmate_common::rules::rule::{RiskLevel, RuleScope, RuleStatus};
use mailmate_common::time::Timestamp;
use mailmate_common::workflow::{
    ExitCondition, FollowUpStep, NewWorkflowDefVersion, NewWorkflowDefinition, NewWorkflowInstance,
    Staleness, WorkflowAnchor, WorkflowConflict, WorkflowConflictKind, WorkflowConflictStatus,
    WorkflowInstanceStatus, WorkflowShadowOutcome, WorkflowVersionContent,
};
use mailmate_ports::storage::feedback::FeedbackRepository;
use mailmate_ports::storage::pipeline_items::PipelineItemRepository;
use mailmate_ports::storage::workflows::{
    WorkflowConflictRepository, WorkflowInstanceRepository, WorkflowRepository,
    WorkflowShadowOutcomeRepository,
};
use mailmate_storage::{
    open_and_migrate, SqliteBackend, SqliteFeedbackRepository, SqlitePipelineItemRepository,
    SqliteWorkflowConflictRepository, SqliteWorkflowInstanceRepository, SqliteWorkflowRepository,
    SqliteWorkflowShadowOutcomeRepository, StorageConfig,
};

fn backend() -> Arc<SqliteBackend> {
    open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap()
}

fn new_item(thread: &str) -> NewPipelineItem {
    NewPipelineItem {
        account_id: "acct_default".to_owned(),
        thread_id: ThreadId::from(thread),
        anchor_message_id: Some(MessageId::from("msg_quote")),
        counterparty_email: "buyer@acme.test".to_owned(),
        counterparty_domain: "acme.test".to_owned(),
        title: "Acme — SCO quote".to_owned(),
        item_type: ItemType::Quote,
        amount_hint: Some("$40k".to_owned()),
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
                forbidden_commitments: vec!["prices".to_owned(), "dates".to_owned()],
            },
            FollowUpStep {
                step_index: 1,
                offset_days: 7,
                draft_intent: "value_add".to_owned(),
                prompt_template_ref: Some("pt_followup_1".to_owned()),
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

fn new_definition() -> NewWorkflowDefinition {
    NewWorkflowDefinition {
        stable_name: "standard-quote-follow-up".to_owned(),
        scope: RuleScope::Global,
        applies_to_item_type: ItemType::Quote,
        created_by: Actor::User,
        initial_version: cadence(),
    }
}

// --- pipeline items -------------------------------------------------------------------

#[test]
fn pipeline_item_insert_get_and_stage_update() {
    let backend = backend();
    let repo = SqlitePipelineItemRepository::new(Arc::clone(&backend));

    let id = block_on(repo.insert(new_item("thread_acme"))).unwrap();
    let item = block_on(repo.get(&id)).unwrap().expect("present");
    assert_eq!(item.stage, PipelineStage::Open, "starts open");
    assert_eq!(item.created_by, Actor::User, "always user-enrolled");
    assert_eq!(item.counterparty_domain, "acme.test");
    assert_eq!(item.anchor_message_id, Some(MessageId::from("msg_quote")));

    block_on(repo.update_stage(&id, PipelineStage::Won)).unwrap();
    let won = block_on(repo.get(&id)).unwrap().unwrap();
    assert_eq!(won.stage, PipelineStage::Won);

    // Lookup by thread and by stage query.
    let by_thread = block_on(repo.get_by_thread(&ThreadId::from("thread_acme"))).unwrap();
    assert_eq!(by_thread.len(), 1);
    let won_items = block_on(repo.query(PipelineItemQuery {
        account_id: Some("acct_default".to_owned()),
        stage: Some(PipelineStage::Won),
        limit: None,
    }))
    .unwrap();
    assert_eq!(won_items.len(), 1);
    assert!(block_on(repo.get(&PipelineItemId::from("pli_missing")))
        .unwrap()
        .is_none());
}

// --- definitions / versions -----------------------------------------------------------

#[test]
fn workflow_definition_draft_and_versioning() {
    let backend = backend();
    let repo = SqliteWorkflowRepository::new(Arc::clone(&backend));

    let wf_id = block_on(repo.save_definition_draft(new_definition())).unwrap();
    let def = block_on(repo.get_definition(&wf_id))
        .unwrap()
        .expect("present");
    assert_eq!(def.status, RuleStatus::Draft);
    assert_eq!(def.stable_name, "standard-quote-follow-up");

    // The pinned version 1 round-trips its cadence content.
    let v1 = block_on(repo.get_version(&def.current_version_id))
        .unwrap()
        .expect("version present");
    assert_eq!(v1.version_number, 1);
    assert_eq!(v1.content.steps.len(), 2);
    assert_eq!(v1.content.steps[0].offset_days, 3);
    assert_eq!(v1.content.anchor, WorkflowAnchor::QuoteSentAt);

    // A new version is monotonic and re-points the definition's current version.
    let mut v2_content = cadence();
    v2_content.steps.push(FollowUpStep {
        step_index: 2,
        offset_days: 14,
        draft_intent: "last_call".to_owned(),
        prompt_template_ref: None,
        forbidden_commitments: vec![],
    });
    v2_content.change_reason = "added day-14 last call".to_owned();
    let v2_id = block_on(repo.create_version(NewWorkflowDefVersion {
        workflow_id: wf_id.clone(),
        content: v2_content,
    }))
    .unwrap();
    let def2 = block_on(repo.get_definition(&wf_id)).unwrap().unwrap();
    assert_eq!(def2.current_version_id, v2_id, "repointed to v2");
    let v2 = block_on(repo.get_version(&v2_id)).unwrap().unwrap();
    assert_eq!(v2.version_number, 2);
    assert_eq!(v2.content.steps.len(), 3);

    // Lifecycle transition + listing by status.
    block_on(repo.update_status(&wf_id, RuleStatus::ShadowMode)).unwrap();
    let shadow = block_on(repo.list_by_status(RuleStatus::ShadowMode)).unwrap();
    assert_eq!(shadow.len(), 1);
    assert!(block_on(repo.list_by_status(RuleStatus::Active))
        .unwrap()
        .is_empty());
    assert!(
        block_on(repo.get_definition(&WorkflowDefId::from("wfd_missing")))
            .unwrap()
            .is_none()
    );
}

// --- instances ------------------------------------------------------------------------

fn arm_one(
    backend: &Arc<SqliteBackend>,
    tag: &str,
    next_due: Option<Timestamp>,
) -> (
    WorkflowInstanceId,
    PipelineItemId,
    WorkflowDefId,
    WorkflowDefVersionId,
) {
    let items = SqlitePipelineItemRepository::new(Arc::clone(backend));
    let workflows = SqliteWorkflowRepository::new(Arc::clone(backend));
    let instances = SqliteWorkflowInstanceRepository::new(Arc::clone(backend));

    let thread = format!("thread_{tag}");
    let item_id = block_on(items.insert(new_item(&thread))).unwrap();
    let mut def_draft = new_definition();
    def_draft.stable_name = format!("standard-quote-follow-up-{tag}");
    let wf_id = block_on(workflows.save_definition_draft(def_draft)).unwrap();
    let def = block_on(workflows.get_definition(&wf_id)).unwrap().unwrap();
    let instance = block_on(instances.arm(NewWorkflowInstance {
        pipeline_item_id: item_id.clone(),
        workflow_id: wf_id.clone(),
        pinned_def_version_id: def.current_version_id.clone(),
        thread_id: ThreadId::from(thread.as_str()),
        anchor_at: Timestamp::now(),
        status: WorkflowInstanceStatus::Active,
        current_step_index: 0,
        next_due_at: next_due,
    }))
    .unwrap();
    (instance, item_id, wf_id, def.current_version_id)
}

#[test]
fn instance_arm_due_listing_and_state_update() {
    let backend = backend();
    let instances = SqliteWorkflowInstanceRepository::new(Arc::clone(&backend));

    let past = Timestamp::parse_rfc3339("2020-01-01T00:00:00Z").unwrap();
    let (inst_id, item_id, _wf, _v) = arm_one(&backend, "acme", Some(past));

    let loaded = block_on(instances.get(&inst_id)).unwrap().expect("present");
    assert_eq!(loaded.status, WorkflowInstanceStatus::Active);
    assert!(loaded.honours_due_invariant());

    // It is due "now" (past < now) and is selected by the drain query.
    let due = block_on(instances.list_due(Timestamp::now(), 100)).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].id, inst_id);

    // A future due time is NOT selected.
    let (_future_inst, _i, _w, _vv) = arm_one(
        &backend,
        "beta",
        Some(Timestamp::parse_rfc3339("2999-01-01T00:00:00Z").unwrap()),
    );
    let due_now = block_on(instances.list_due(Timestamp::now(), 100)).unwrap();
    assert_eq!(due_now.len(), 1, "only the overdue instance is due");

    // Moving to awaiting_review clears next_due_at → no longer selected.
    block_on(instances.update_state(&inst_id, WorkflowInstanceStatus::AwaitingReview, 1, None))
        .unwrap();
    let after = block_on(instances.get(&inst_id)).unwrap().unwrap();
    assert_eq!(after.status, WorkflowInstanceStatus::AwaitingReview);
    assert_eq!(after.current_step_index, 1);
    assert!(after.next_due_at.is_none());
    assert!(after.honours_due_invariant());
    let still_due = block_on(instances.list_due(Timestamp::now(), 100)).unwrap();
    assert!(
        !still_due.iter().any(|i| i.id == inst_id),
        "awaiting_review instance is not drained"
    );

    // Reply-exit / item lookups.
    let by_thread =
        block_on(instances.list_active_by_thread(&ThreadId::from("thread_acme"))).unwrap();
    assert!(by_thread.iter().any(|i| i.id == inst_id));
    let by_item = block_on(instances.list_by_pipeline_item(&item_id)).unwrap();
    assert_eq!(by_item.len(), 1);
}

#[test]
fn due_comparison_is_exact_at_the_sub_second_boundary() {
    let backend = backend();
    let items = SqlitePipelineItemRepository::new(Arc::clone(&backend));
    let workflows = SqliteWorkflowRepository::new(Arc::clone(&backend));
    let instances = SqliteWorkflowInstanceRepository::new(Arc::clone(&backend));

    let item_id = block_on(items.insert(new_item("thread_boundary"))).unwrap();
    let mut def_draft = new_definition();
    def_draft.stable_name = "wf-boundary".to_owned();
    let wf_id = block_on(workflows.save_definition_draft(def_draft)).unwrap();
    let def = block_on(workflows.get_definition(&wf_id)).unwrap().unwrap();
    // A step due exactly at a whole second.
    let due = Timestamp::parse_rfc3339("2026-06-20T00:00:00Z").unwrap();
    let inst = block_on(instances.arm(NewWorkflowInstance {
        pipeline_item_id: item_id,
        workflow_id: wf_id,
        pinned_def_version_id: def.current_version_id,
        thread_id: ThreadId::from("thread_boundary"),
        anchor_at: due,
        status: WorkflowInstanceStatus::Active,
        current_step_index: 0,
        next_due_at: Some(due),
    }))
    .unwrap();
    // `now` is 0.5s later — the SAME whole second. The step is due and MUST be selected
    // (a naive RFC3339 TEXT compare would mis-order `…00Z` vs `…00.5Z` and drop it).
    let now = Timestamp::parse_rfc3339("2026-06-20T00:00:00.5Z").unwrap();
    let due_now = block_on(instances.list_due(now, 100)).unwrap();
    assert!(
        due_now.iter().any(|i| i.id == inst),
        "a whole-second due step is selected when now is later within the same second"
    );
}

#[test]
fn list_due_is_bounded_by_the_limit_and_returns_the_soonest_first() {
    let backend = backend();
    let instances = SqliteWorkflowInstanceRepository::new(Arc::clone(&backend));
    // Five instances due at staggered, ascending times (all in the past relative to `now`).
    for i in 0..5 {
        let due = Timestamp::parse_rfc3339(&format!("2026-06-20T00:0{i}:00Z")).unwrap();
        arm_one(&backend, &format!("cap{i}"), Some(due));
    }
    let now = Timestamp::parse_rfc3339("2026-06-21T00:00:00Z").unwrap();

    // The cap bounds the batch to the three soonest-due instances.
    let batch = block_on(instances.list_due(now, 3)).unwrap();
    assert_eq!(batch.len(), 3, "the batch cap bounds the drain");
    assert!(
        batch.windows(2).all(|w| w[0].next_due_at <= w[1].next_due_at),
        "ordered soonest-due first"
    );
    // A zero cap drains nothing; a generous cap returns all five.
    assert!(block_on(instances.list_due(now, 0)).unwrap().is_empty());
    assert_eq!(block_on(instances.list_due(now, 100)).unwrap().len(), 5);
}

#[test]
fn terminal_instances_are_excluded_from_reply_exit_lookup() {
    let backend = backend();
    let instances = SqliteWorkflowInstanceRepository::new(Arc::clone(&backend));
    let (inst_id, _item, _wf, _v) = arm_one(&backend, "acme", Some(Timestamp::now()));
    block_on(instances.update_state(&inst_id, WorkflowInstanceStatus::Completed, 2, None)).unwrap();
    let active = block_on(instances.list_active_by_thread(&ThreadId::from("thread_acme"))).unwrap();
    assert!(active.is_empty(), "completed instances are not active");
}

#[test]
fn arming_an_instance_without_its_pipeline_item_violates_a_foreign_key() {
    let backend = backend();
    let instances = SqliteWorkflowInstanceRepository::new(Arc::clone(&backend));
    let workflows = SqliteWorkflowRepository::new(Arc::clone(&backend));
    let wf_id = block_on(workflows.save_definition_draft(new_definition())).unwrap();
    let def = block_on(workflows.get_definition(&wf_id)).unwrap().unwrap();

    let err = block_on(instances.arm(NewWorkflowInstance {
        pipeline_item_id: PipelineItemId::from("pli_ghost"),
        workflow_id: wf_id,
        pinned_def_version_id: def.current_version_id,
        thread_id: ThreadId::from("thread_x"),
        anchor_at: Timestamp::now(),
        status: WorkflowInstanceStatus::Active,
        current_step_index: 0,
        next_due_at: Some(Timestamp::now()),
    }))
    .unwrap_err();
    assert!(matches!(err, StorageError::Constraint(_)), "got {err:?}");
}

// --- follow-up feedback ---------------------------------------------------------------

#[test]
fn followup_feedback_round_trips_with_coalesced_steps() {
    let backend = backend();
    let repo: SqliteFeedbackRepository = SqliteFeedbackRepository::new(Arc::clone(&backend));

    let row = FollowUpFeedbackRow {
        id: FollowUpFeedback::fresh_id(),
        workflow_instance_id: WorkflowInstanceId::from("wfi_1"),
        pipeline_item_id: PipelineItemId::from("pli_1"),
        step_index: 2,
        draft_id: Some(DraftId::from("draft_1")),
        pinned_versions: PinnedVersions::default(),
        ai_scheduled_offset_days: 14,
        actual_offset_days: None,
        reply_received_before_step: false,
        reply_latency_days: None,
        outcome: FollowUpOutcome::SurfacedForReview,
        coalesced_from: vec![1],
        human_reason_code: None,
        human_reason_text: None,
        polarity: FeedbackPolarity::Positive,
        created_at: Timestamp::now(),
    };
    block_on(FeedbackRepository::<FollowUpFeedback>::append(
        &repo,
        row.clone(),
    ))
    .unwrap();

    let found = block_on(FeedbackRepository::<FollowUpFeedback>::query(
        &repo,
        FollowUpFeedbackQuery {
            workflow_instance_id: Some(WorkflowInstanceId::from("wfi_1")),
            pipeline_item_id: None,
            limit: None,
        },
    ))
    .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].coalesced_from, vec![1]);
    assert_eq!(found[0].outcome, FollowUpOutcome::SurfacedForReview);
    assert_eq!(found[0].draft_id, Some(DraftId::from("draft_1")));
}

// --- conflicts ------------------------------------------------------------------------

#[test]
fn workflow_conflict_append_list_and_resolve() {
    let backend = backend();
    let repo = SqliteWorkflowConflictRepository::new(Arc::clone(&backend));
    let id = WorkflowConflictId::fresh();
    block_on(repo.append(WorkflowConflict {
        id: id.clone(),
        pipeline_item_id: PipelineItemId::from("pli_1"),
        workflow_a_id: "wfi_a".to_owned(),
        workflow_b_id: "wfd_b".to_owned(),
        conflict_kind: WorkflowConflictKind::ConcurrentActiveWorkflow,
        status: WorkflowConflictStatus::Open,
        detected_at: Timestamp::now(),
    }))
    .unwrap();
    let open = block_on(repo.list_open()).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].workflow_a_id, "wfi_a");
    block_on(repo.resolve(&id)).unwrap();
    assert!(block_on(repo.list_open()).unwrap().is_empty(), "resolved");
}

// --- shadow outcomes ------------------------------------------------------------------

#[test]
fn workflow_shadow_outcome_append_and_list() {
    let backend = backend();
    let repo = SqliteWorkflowShadowOutcomeRepository::new(Arc::clone(&backend));
    let workflow_id = WorkflowDefId::from("wfd_shadow");
    block_on(repo.append(WorkflowShadowOutcome {
        id: WorkflowShadowOutcomeId::fresh(),
        workflow_id: workflow_id.clone(),
        workflow_version_id: WorkflowDefVersionId::from("wfdv_1"),
        pipeline_item_id: PipelineItemId::from("pli_1"),
        thread_id: ThreadId::from("thread_1"),
        step_index: 0,
        would_fire_at: Timestamp::now(),
        reply_before_fire: true,
        matched_manual_followup_within_days: Some(2),
        created_at: Timestamp::now(),
    }))
    .unwrap();
    let rows = block_on(repo.list_for_workflow(&workflow_id)).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].reply_before_fire);
    assert_eq!(rows[0].matched_manual_followup_within_days, Some(2));
}
