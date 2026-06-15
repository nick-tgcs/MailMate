//! Integration tests for the agent curator and the proposal-review step, driven through the
//! real `RuleCurator`/`ProposalReview` adapters over a migrated in-memory backend with a
//! deterministic mock provider. They prove the safety invariants Phase 8 is responsible for:
//! the curator may *propose* but never *activate*; risky and conflicting candidates are forced
//! to human review; live-rule conflicts are detected and recorded; and a rule is materialized
//! ONLY by an accepting human review, into shadow/pending — never active.

use std::sync::Arc;

use futures::executor::block_on;
use serde_json::{json, Value};

use mailmate_ai::providers::MockProvider;
use mailmate_common::actor::Actor;
use mailmate_common::audit::{event_type, AuditQuery};
use mailmate_common::conflict::ConflictStatus;
use mailmate_common::curator::{CuratorOperation, CuratorRequest, ReviewDecision};
use mailmate_common::error::{AiError, CuratorError, ReviewError};
use mailmate_common::features::FeatureVector;
use mailmate_common::feedback::{
    ClassificationFeedback, ClassificationFeedbackRow, FeedbackPolarity, PinnedVersions,
    ProposalOutcome, RuleProposalFeedback, RuleProposalFeedbackQuery,
};
use mailmate_common::ids::{MessageId, ProposalId, RuleId, RuleVersionId};
use mailmate_common::proposal::{AgentProposal, ProposalKind, ProposalStatus};
use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::rule::{
    EvaluatableRule, HierarchyBand, NewRule, RiskLevel, RuleDraft, RuleKind, RuleScope, RuleStatus,
    RuleVersion, RuleVersionContent,
};
use mailmate_common::time::Timestamp;
use mailmate_learning::{AiRuleCurator, DefaultLearningEngine, DefaultProposalReview};
use mailmate_ports::ai_provider::AiProvider;
use mailmate_ports::proposal_review::ProposalReview;
use mailmate_ports::rule_curator::RuleCurator;
use mailmate_ports::storage::audit::AuditRepository;
use mailmate_ports::storage::conflicts::ConflictRepository;
use mailmate_ports::storage::feedback::FeedbackRepository;
use mailmate_ports::storage::proposals::ProposalRepository;
use mailmate_ports::storage::rules::RuleRepository;
use mailmate_rules::DeterministicRuleEngine;
use mailmate_storage::{
    open_and_migrate, SqliteAuditRepository, SqliteBackend, SqliteConflictRepository,
    SqliteFeedbackRepository, SqliteProposalRepository, SqliteRuleRepository,
    SqliteWorkflowRepository, StorageConfig,
};

/// The shared real repositories over one migrated backend.
struct Repos {
    rules: Arc<SqliteRuleRepository>,
    workflows: Arc<SqliteWorkflowRepository>,
    proposals: Arc<SqliteProposalRepository>,
    conflicts: Arc<SqliteConflictRepository>,
    audit: Arc<SqliteAuditRepository>,
    feedback: Arc<SqliteFeedbackRepository>,
}

fn repos() -> Repos {
    let backend: Arc<SqliteBackend> = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    Repos {
        rules: Arc::new(SqliteRuleRepository::new(Arc::clone(&backend))),
        workflows: Arc::new(SqliteWorkflowRepository::new(Arc::clone(&backend))),
        proposals: Arc::new(SqliteProposalRepository::new(Arc::clone(&backend))),
        conflicts: Arc::new(SqliteConflictRepository::new(Arc::clone(&backend))),
        audit: Arc::new(SqliteAuditRepository::new(Arc::clone(&backend))),
        feedback: Arc::new(SqliteFeedbackRepository::new(Arc::clone(&backend))),
    }
}

fn learning(repos: &Repos) -> Arc<DefaultLearningEngine> {
    Arc::new(DefaultLearningEngine::new(
        repos.feedback.clone(),
        repos.feedback.clone(),
        repos.audit.clone(),
        repos.proposals.clone(),
    ))
}

fn curator(
    repos: &Repos,
    provider: Arc<dyn AiProvider>,
    engine_rules: Vec<EvaluatableRule>,
) -> AiRuleCurator {
    AiRuleCurator::new(
        provider,
        learning(repos),
        repos.rules.clone(),
        Arc::new(DeterministicRuleEngine::new(engine_rules)),
        repos.proposals.clone(),
        repos.conflicts.clone(),
        repos.audit.clone(),
    )
}

fn review(repos: &Repos) -> DefaultProposalReview {
    DefaultProposalReview::new(
        repos.proposals.clone(),
        repos.rules.clone(),
        repos.workflows.clone(),
        repos.feedback.clone(),
        repos.audit.clone(),
    )
}

fn sender_domain_eq(domain: &str) -> Condition {
    Condition::Predicate(Predicate {
        field: "sender_domain".to_owned(),
        op: Operator::Eq,
        value: FieldValue::Text(domain.to_owned()),
    })
}

/// A mock provider returning one new-rule proposal at the given risk + recommendation, whose
/// draft moves `domain` mail to `folder`.
fn proposing_provider(
    risk: &str,
    recommended: &str,
    domain: &str,
    folder: &str,
) -> Arc<dyn AiProvider> {
    let value: Value = json!({
        "proposals": [{
            "proposal_type": "new_rule",
            "risk_level": risk,
            "title": format!("File {domain} to {folder}"),
            "rationale": format!("repeated moves from {domain}"),
            "recommended_status": recommended,
            "rule_draft": {
                "kind": "action",
                "scope": "domain",
                "condition": { "field": "sender_domain", "op": "eq", "value": domain },
                "effect": { "move": folder }
            }
        }],
        "threshold_suggestions": [],
        "rationale": "clustered the moves"
    });
    Arc::new(MockProvider::returning_json("mock", value))
}

fn version_content(condition: Condition, folder: &str) -> RuleVersionContent {
    RuleVersionContent {
        title: "f".to_owned(),
        description: "d".to_owned(),
        condition,
        effect: RuleEffect {
            move_to: Some(folder.to_owned()),
            ..RuleEffect::new()
        },
        priority: 0,
        confidence_threshold: None,
        risk_level: RiskLevel::Low,
        change_reason: "seed".to_owned(),
        created_by: Actor::User,
    }
}

fn active_rule(rule_id: &str, domain: &str, folder: &str) -> EvaluatableRule {
    EvaluatableRule {
        rule_id: RuleId::from(rule_id),
        kind: RuleKind::Action,
        scope: RuleScope::Domain,
        band: HierarchyBand::LearnedActive,
        status: RuleStatus::Active,
        version: RuleVersion {
            id: RuleVersionId::from("rv_seed"),
            version_number: 1,
            condition: sender_domain_eq(domain),
            effect: RuleEffect {
                move_to: Some(folder.to_owned()),
                ..RuleEffect::new()
            },
            risk_level: RiskLevel::Low,
        },
    }
}

#[test]
fn curator_proposes_a_pending_shadow_rule_and_never_activates() {
    let repos = repos();
    let curator = curator(
        &repos,
        proposing_provider("low", "shadow_mode", "stripe.com", "Receipts"),
        vec![],
    );

    let report = block_on(curator.curate(CuratorRequest::just(CuratorOperation::Propose))).unwrap();
    assert_eq!(report.proposals.len(), 1);
    let proposal = &report.proposals[0];
    assert_eq!(proposal.status, ProposalStatus::PendingReview);
    assert_eq!(proposal.recommended_status, RuleStatus::ShadowMode);
    assert_ne!(
        proposal.recommended_status,
        RuleStatus::Active,
        "the curator must never recommend an active rule"
    );

    // It is persisted as pending, and the emission was audited.
    let pending = block_on(
        repos
            .proposals
            .list_by_status(ProposalStatus::PendingReview),
    )
    .unwrap();
    assert_eq!(pending.len(), 1);
    let proposed = block_on(repos.audit.query(AuditQuery {
        event_type: Some(event_type::RULE_PROPOSED.to_owned()),
        ..AuditQuery::default()
    }))
    .unwrap();
    assert_eq!(proposed.len(), 1);
    assert_eq!(proposed[0].actor, Actor::Ai);
}

#[test]
fn curator_forces_a_risky_proposal_to_human_review() {
    let repos = repos();
    // The model cautiously says shadow_mode, but the proposal is high risk.
    let curator = curator(
        &repos,
        proposing_provider("high", "shadow_mode", "irs-refund.example", "Inbox"),
        vec![],
    );
    let report = block_on(curator.curate(CuratorRequest::just(CuratorOperation::Propose))).unwrap();
    assert_eq!(
        report.proposals[0].recommended_status,
        RuleStatus::PendingHumanReview,
        "a risky proposal is clamped to human review whatever the model said"
    );
}

#[test]
fn curator_forces_a_conflicting_candidate_to_human_review() {
    let repos = repos();
    // An existing active rule files paypal.com to Inbox; the candidate would file it to Trash.
    let engine_rules = vec![active_rule("rule_existing", "paypal.com", "Inbox")];
    let curator = curator(
        &repos,
        proposing_provider("low", "shadow_mode", "paypal.com", "Trash"),
        engine_rules,
    );
    let report = block_on(curator.curate(CuratorRequest::just(CuratorOperation::Propose))).unwrap();
    let proposal = &report.proposals[0];
    assert_eq!(
        proposal.recommended_status,
        RuleStatus::PendingHumanReview,
        "a candidate that contradicts a live rule needs human review"
    );
    assert!(
        proposal.rationale.contains("conflict"),
        "the conflict is surfaced in the rationale: {}",
        proposal.rationale
    );
}

#[test]
fn curator_records_a_provider_failure_as_audit_and_errors() {
    let repos = repos();
    let provider: Arc<dyn AiProvider> = Arc::new(MockProvider::failing(
        "mock",
        AiError::RequestFailed {
            code: "503".to_owned(),
            message: "down".to_owned(),
        },
    ));
    let curator = curator(&repos, provider, vec![]);
    let err =
        block_on(curator.curate(CuratorRequest::just(CuratorOperation::Propose))).unwrap_err();
    assert!(matches!(err, CuratorError::Provider(_)), "got {err:?}");

    // The invalid/failed response is audited and drove no proposal.
    let rejected = block_on(repos.audit.query(AuditQuery {
        event_type: Some(event_type::PROVIDER_RESPONSE_REJECTED.to_owned()),
        ..AuditQuery::default()
    }))
    .unwrap();
    assert_eq!(rejected.len(), 1);
    assert!(block_on(
        repos
            .proposals
            .list_by_status(ProposalStatus::PendingReview)
    )
    .unwrap()
    .is_empty());
}

#[test]
fn curator_detects_and_records_conflicts_between_live_rules() {
    let repos = repos();
    // Two ACTIVE action rules share the same domain condition but file to different folders.
    for (name, folder) in [("rule_a", "Inbox"), ("rule_b", "Trash")] {
        let id = block_on(repos.rules.save_rule_draft(NewRule {
            stable_name: name.to_owned(),
            kind: RuleKind::Action,
            scope: RuleScope::Domain,
            band: HierarchyBand::LearnedActive,
            created_by: Actor::User,
            initial_version: version_content(sender_domain_eq("acme.com"), folder),
        }))
        .unwrap();
        block_on(
            repos
                .rules
                .update_rule_status(&id, RuleKind::Action, RuleStatus::Active),
        )
        .unwrap();
    }

    // A provider that would error if called — DetectConflicts is deterministic, no AI.
    let provider: Arc<dyn AiProvider> = Arc::new(MockProvider::failing(
        "mock",
        AiError::Unavailable("unused".to_owned()),
    ));
    let curator = curator(&repos, provider, vec![]);

    let report =
        block_on(curator.curate(CuratorRequest::just(CuratorOperation::DetectConflicts))).unwrap();
    assert_eq!(report.conflicts.len(), 1, "the contradictory pair is found");
    let open = block_on(repos.conflicts.list_open()).unwrap();
    assert_eq!(open.len(), 1, "and recorded in rule_conflicts");
    assert_eq!(open[0].status, ConflictStatus::Open);
}

#[test]
fn review_accepting_a_proposal_creates_a_shadow_rule_and_records_acceptance() {
    let repos = repos();
    // Curate a low-risk shadow proposal, then accept it.
    let curator = curator(
        &repos,
        proposing_provider("low", "shadow_mode", "stripe.com", "Receipts"),
        vec![],
    );
    let report = block_on(curator.curate(CuratorRequest::just(CuratorOperation::Propose))).unwrap();
    let proposal_id = report.proposals[0].id.clone();

    let review = review(&repos);
    let outcome = block_on(review.review(ReviewDecision::accept(proposal_id.clone()))).unwrap();
    assert_eq!(outcome.new_status, ProposalStatus::Accepted);
    let rule_id = outcome
        .created_rule_id
        .expect("a rule is created on acceptance");

    // The rule entered SHADOW, not active.
    assert_eq!(
        block_on(
            repos
                .rules
                .get_shadow_rules(RuleKind::Action, RuleScope::Domain)
        )
        .unwrap()
        .len(),
        1,
        "the accepted rule is shadow-tested"
    );
    assert!(
        block_on(
            repos
                .rules
                .get_active_rules(RuleKind::Action, RuleScope::Domain)
        )
        .unwrap()
        .is_empty(),
        "acceptance never activates a rule directly"
    );

    // The decision was recorded to rule_proposal_feedback (positive) and audited.
    let fb = block_on(FeedbackRepository::<RuleProposalFeedback>::query(
        repos.feedback.as_ref(),
        RuleProposalFeedbackQuery {
            proposal_id: Some(proposal_id.clone()),
            ..RuleProposalFeedbackQuery::default()
        },
    ))
    .unwrap();
    assert_eq!(fb.len(), 1);
    assert_eq!(fb[0].outcome, ProposalOutcome::Accepted);

    let reviewed = block_on(repos.audit.query(AuditQuery {
        event_type: Some(event_type::PROPOSAL_REVIEWED.to_owned()),
        ..AuditQuery::default()
    }))
    .unwrap();
    assert_eq!(reviewed.len(), 1);
    assert_eq!(reviewed[0].actor, Actor::User);
    // And the rule id round-trips through the review outcome.
    assert!(rule_id.as_str().starts_with("rule_"));
    // It is no longer pending.
    assert!(block_on(review.pending()).unwrap().is_empty());
}

#[test]
fn review_rejecting_a_proposal_records_negative_feedback_and_creates_no_rule() {
    let repos = repos();
    let curator = curator(
        &repos,
        proposing_provider("low", "shadow_mode", "spam.example", "Trash"),
        vec![],
    );
    let report = block_on(curator.curate(CuratorRequest::just(CuratorOperation::Propose))).unwrap();
    let proposal_id = report.proposals[0].id.clone();

    let review = review(&repos);
    let outcome =
        block_on(review.review(ReviewDecision::reject(proposal_id.clone(), "too_broad"))).unwrap();
    assert_eq!(outcome.new_status, ProposalStatus::Rejected);
    assert!(
        outcome.created_rule_id.is_none(),
        "rejection creates no rule"
    );
    assert!(block_on(
        repos
            .rules
            .get_shadow_rules(RuleKind::Action, RuleScope::Domain)
    )
    .unwrap()
    .is_empty());

    let fb = block_on(FeedbackRepository::<RuleProposalFeedback>::query(
        repos.feedback.as_ref(),
        RuleProposalFeedbackQuery::default(),
    ))
    .unwrap();
    assert_eq!(fb.len(), 1);
    assert_eq!(fb[0].outcome, ProposalOutcome::Rejected);
    assert_eq!(
        fb[0].human_reason_code.as_deref(),
        Some("too_broad"),
        "the rejection reason is captured"
    );
}

#[test]
fn review_rejects_unknown_and_already_reviewed_proposals() {
    let repos = repos();
    let review = review(&repos);

    // Unknown id.
    let err = block_on(review.review(ReviewDecision::accept(ProposalId::from("prop_absent"))))
        .unwrap_err();
    assert!(matches!(err, ReviewError::NotFound(_)), "got {err:?}");

    // A proposal accepted once cannot be re-decided.
    let curator = curator(
        &repos,
        proposing_provider("low", "shadow_mode", "stripe.com", "Receipts"),
        vec![],
    );
    let report = block_on(curator.curate(CuratorRequest::just(CuratorOperation::Propose))).unwrap();
    let proposal_id = report.proposals[0].id.clone();
    block_on(review.review(ReviewDecision::accept(proposal_id.clone()))).unwrap();
    let err =
        block_on(review.review(ReviewDecision::reject(proposal_id, "changed_mind"))).unwrap_err();
    assert!(
        matches!(err, ReviewError::AlreadyReviewed(_)),
        "got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Richer review + curator coverage
// ---------------------------------------------------------------------------

/// Persist a real active rule and return its id (so refine/retire targets exist).
fn seed_active_rule(repos: &Repos, name: &str, kind: RuleKind, folder: &str) -> RuleId {
    let id = block_on(repos.rules.save_rule_draft(NewRule {
        stable_name: name.to_owned(),
        kind,
        scope: RuleScope::Domain,
        band: HierarchyBand::LearnedActive,
        created_by: Actor::User,
        initial_version: version_content(sender_domain_eq("acme.com"), folder),
    }))
    .unwrap();
    block_on(
        repos
            .rules
            .update_rule_status(&id, kind, RuleStatus::Active),
    )
    .unwrap();
    id
}

/// A hand-built proposal persisted as pending (for kinds the curator emits only via the AI).
fn persist_proposal(
    repos: &Repos,
    proposal_type: ProposalKind,
    rule_draft: Option<RuleDraft>,
    target_rule_id: Option<RuleId>,
    target_rule_kind: Option<RuleKind>,
) -> ProposalId {
    let id = ProposalId::fresh();
    let proposal = AgentProposal {
        id: id.clone(),
        proposal_type,
        status: ProposalStatus::PendingReview,
        title: "t".to_owned(),
        rationale: "r".to_owned(),
        risk_level: RiskLevel::Low,
        recommended_status: RuleStatus::ShadowMode,
        rule_draft,
        target_rule_kind,
        target_rule_id,
        workflow_draft: None,
        target_workflow_id: None,
        evidence_refs: Vec::new(),
        source_provider: "test".to_owned(),
        created_at: Timestamp::now(),
        reviewed_at: None,
    };
    block_on(repos.proposals.save(proposal, Vec::new())).unwrap();
    id
}

/// Persist a `new_workflow` proposal (pending) carrying a candidate cadence.
fn persist_workflow_proposal(repos: &Repos) -> ProposalId {
    use mailmate_common::pipeline::ItemType;
    use mailmate_common::workflow::{
        ExitCondition, FollowUpStep, Staleness, WorkflowAnchor, WorkflowDraft,
    };
    let id = ProposalId::fresh();
    let proposal = AgentProposal {
        id: id.clone(),
        proposal_type: ProposalKind::NewWorkflow,
        status: ProposalStatus::PendingReview,
        title: "Standard quote follow-up".to_owned(),
        rationale: "repeated manual follow-ups at day 3 / 7".to_owned(),
        risk_level: RiskLevel::Medium,
        recommended_status: RuleStatus::ShadowMode,
        rule_draft: None,
        target_rule_kind: None,
        target_rule_id: None,
        workflow_draft: Some(WorkflowDraft {
            stable_name: "standard-quote-follow-up".to_owned(),
            scope: RuleScope::Global,
            applies_to_item_type: ItemType::Quote,
            anchor: WorkflowAnchor::QuoteSentAt,
            steps: vec![FollowUpStep {
                step_index: 0,
                offset_days: 3,
                draft_intent: "gentle_check_in".to_owned(),
                prompt_template_ref: None,
                forbidden_commitments: vec!["prices".to_owned()],
            }],
            exit_conditions: vec![ExitCondition::ReplyReceived],
            staleness: Staleness::default(),
            risk_level: RiskLevel::Medium,
        }),
        target_workflow_id: None,
        evidence_refs: Vec::new(),
        source_provider: "test".to_owned(),
        created_at: Timestamp::now(),
        reviewed_at: None,
    };
    block_on(repos.proposals.save(proposal, Vec::new())).unwrap();
    id
}

#[test]
fn review_accepting_a_new_workflow_creates_a_shadow_workflow_never_active() {
    use mailmate_ports::storage::workflows::WorkflowRepository;
    let repos = repos();
    let proposal_id = persist_workflow_proposal(&repos);

    let review = review(&repos);
    let outcome = block_on(review.review(ReviewDecision::accept(proposal_id))).unwrap();
    assert_eq!(outcome.new_status, ProposalStatus::Accepted);
    assert!(
        outcome.created_rule_id.is_none(),
        "a workflow acceptance creates no rule"
    );

    // The workflow entered SHADOW, not active.
    let shadow = block_on(repos.workflows.list_by_status(RuleStatus::ShadowMode)).unwrap();
    assert_eq!(shadow.len(), 1, "the accepted workflow is shadow-tested");
    assert!(
        block_on(repos.workflows.list_by_status(RuleStatus::Active))
            .unwrap()
            .is_empty(),
        "acceptance never activates a workflow directly"
    );
    // Its pinned version round-trips the proposed cadence.
    let def = &shadow[0];
    let v1 = block_on(repos.workflows.get_version(&def.current_version_id))
        .unwrap()
        .unwrap();
    assert_eq!(v1.content.steps.len(), 1);
    assert_eq!(v1.content.steps[0].offset_days, 3);

    // The materialization was audited as a workflow status change.
    let audited = block_on(repos.audit.query(AuditQuery {
        event_type: Some("workflow_status_changed".to_owned()),
        ..AuditQuery::default()
    }))
    .unwrap();
    assert_eq!(audited.len(), 1);
}

fn domain_draft(kind: RuleKind, folder: &str) -> RuleDraft {
    RuleDraft {
        kind,
        scope: RuleScope::Domain,
        condition: sender_domain_eq("acme.com"),
        effect: RuleEffect {
            move_to: Some(folder.to_owned()),
            ..RuleEffect::new()
        },
    }
}

#[test]
fn review_accepting_a_refine_appends_a_new_version_to_the_target() {
    let repos = repos();
    let target = seed_active_rule(&repos, "invoice", RuleKind::Action, "Inbox");
    let proposal_id = persist_proposal(
        &repos,
        ProposalKind::RefineRule,
        Some(domain_draft(RuleKind::Action, "Invoices")),
        Some(target.clone()),
        Some(RuleKind::Action),
    );

    let review = review(&repos);
    let outcome = block_on(review.review(ReviewDecision::accept(proposal_id))).unwrap();
    assert_eq!(outcome.new_status, ProposalStatus::Accepted);
    assert!(
        outcome.created_rule_id.is_none(),
        "a refine creates no new rule"
    );

    // The target rule now reflects version 2 with the refined effect.
    let active = block_on(
        repos
            .rules
            .get_active_rules(RuleKind::Action, RuleScope::Domain),
    )
    .unwrap();
    let refined = active.iter().find(|r| r.rule_id == target).unwrap();
    assert_eq!(refined.version.version_number, 2);
    assert_eq!(refined.version.effect.move_to.as_deref(), Some("Invoices"));
}

#[test]
fn review_accepting_a_retire_retires_the_target_rule() {
    let repos = repos();
    let target = seed_active_rule(&repos, "stale", RuleKind::Action, "Inbox");
    let proposal_id = persist_proposal(
        &repos,
        ProposalKind::RetireRule,
        None,
        Some(target.clone()),
        Some(RuleKind::Action),
    );

    let review = review(&repos);
    let outcome = block_on(review.review(ReviewDecision::accept(proposal_id))).unwrap();
    assert_eq!(outcome.new_status, ProposalStatus::Accepted);
    assert!(
        block_on(
            repos
                .rules
                .get_active_rules(RuleKind::Action, RuleScope::Domain)
        )
        .unwrap()
        .is_empty(),
        "the retired rule leaves the active snapshot"
    );
}

#[test]
fn review_accepting_a_merge_records_the_decision_without_editing_rules() {
    let repos = repos();
    let target = seed_active_rule(&repos, "broad", RuleKind::Action, "Inbox");
    let proposal_id = persist_proposal(
        &repos,
        ProposalKind::MergeRules,
        None,
        Some(target.clone()),
        Some(RuleKind::Action),
    );

    let review = review(&repos);
    let outcome = block_on(review.review(ReviewDecision::accept(proposal_id))).unwrap();
    assert_eq!(outcome.new_status, ProposalStatus::Accepted);
    assert!(outcome.created_rule_id.is_none());
    // The target rule is untouched (still active).
    assert_eq!(
        block_on(
            repos
                .rules
                .get_active_rules(RuleKind::Action, RuleScope::Domain)
        )
        .unwrap()
        .len(),
        1
    );
}

#[test]
fn review_accepting_a_new_rule_with_no_draft_is_a_missing_draft_error() {
    let repos = repos();
    let proposal_id = persist_proposal(&repos, ProposalKind::NewRule, None, None, None);
    let review = review(&repos);
    let err = block_on(review.review(ReviewDecision::accept(proposal_id))).unwrap_err();
    assert!(matches!(err, ReviewError::MissingDraft(_)), "got {err:?}");
}

#[test]
fn curator_full_pass_proposes_suggests_summarizes_and_detects_conflicts() {
    let repos = repos();

    // Seed evidence so the feedback summary has a non-empty, classification-dominant source.
    block_on(FeedbackRepository::<ClassificationFeedback>::append(
        repos.feedback.as_ref(),
        ClassificationFeedbackRow {
            id: ClassificationFeedback::fresh_id(),
            message_id: MessageId::from("msg_e1"),
            pinned_versions: PinnedVersions::default(),
            ai_label: Some("not_junk".to_owned()),
            ai_score: None,
            ai_rationale: None,
            human_label: "phishing".to_owned(),
            human_reason_code: None,
            human_reason_text: None,
            salient_features: FeatureVector::new(),
            polarity: FeedbackPolarity::Negative,
            created_at: Timestamp::now(),
        },
    ))
    .unwrap();

    // Seed two contradictory live rules so the deterministic scan finds a conflict, plus a
    // labels-effect classification rule so the rule summary exercises that branch.
    seed_active_rule(&repos, "file_a", RuleKind::Action, "Inbox");
    seed_active_rule(&repos, "file_b", RuleKind::Action, "Trash");
    let labels_rule = NewRule {
        stable_name: "label_acme".to_owned(),
        kind: RuleKind::Classification,
        scope: RuleScope::Domain,
        band: HierarchyBand::LearnedActive,
        created_by: Actor::User,
        initial_version: RuleVersionContent {
            title: "l".to_owned(),
            description: "d".to_owned(),
            condition: sender_domain_eq("acme.com"),
            effect: RuleEffect {
                set_labels: vec!["vendor".to_owned()],
                ..RuleEffect::new()
            },
            priority: 0,
            confidence_threshold: None,
            risk_level: RiskLevel::Low,
            change_reason: "seed".to_owned(),
            created_by: Actor::User,
        },
    };
    let cls_id = block_on(repos.rules.save_rule_draft(labels_rule)).unwrap();
    block_on(
        repos
            .rules
            .update_rule_status(&cls_id, RuleKind::Classification, RuleStatus::Active),
    )
    .unwrap();

    // The provider returns a proposal, a threshold suggestion, and a feedback summary.
    let value: Value = json!({
        "proposals": [{
            "proposal_type": "new_rule",
            "risk_level": "low",
            "title": "File widgets.example to Vendors",
            "rationale": "repeated moves",
            "recommended_status": "shadow_mode",
            "rule_draft": {
                "kind": "action",
                "scope": "domain",
                "condition": { "field": "sender_domain", "op": "eq", "value": "widgets.example" },
                "effect": { "move": "Vendors" }
            }
        }],
        "threshold_suggestions": [{
            "rule_kind": "action",
            "source_kind": "filing",
            "suggested": 5,
            "rationale": "filing moves are noisy lately"
        }],
        "feedback_summary": "Mostly phishing corrections this week.",
        "rationale": "a full pass"
    });
    let provider: Arc<dyn AiProvider> = Arc::new(MockProvider::returning_json("mock", value));
    let curator = curator(&repos, provider, vec![]);

    let report = block_on(curator.curate(CuratorRequest::full())).unwrap();
    assert_eq!(report.proposals.len(), 1, "the proposal is persisted");
    assert_eq!(report.threshold_suggestions.len(), 1, "thresholds surfaced");
    assert_eq!(report.threshold_suggestions[0].suggested, 5);
    assert_eq!(report.feedback_summaries.len(), 1, "feedback summarized");
    assert_eq!(
        report.feedback_summaries[0].text,
        "Mostly phishing corrections this week."
    );
    assert_eq!(
        report.conflicts.len(),
        1,
        "the live-rule conflict is recorded"
    );
    assert!(!report.is_empty());
}

#[test]
fn curator_focused_request_keeps_only_the_requested_proposal_kinds() {
    let repos = repos();
    // A target rule for the refine proposal to reference.
    let target = seed_active_rule(&repos, "invoice", RuleKind::Action, "Inbox");
    let value: Value = json!({
        "proposals": [
            {
                "proposal_type": "new_rule",
                "risk_level": "low",
                "title": "new",
                "rationale": "r",
                "recommended_status": "shadow_mode",
                "rule_draft": {
                    "kind": "action", "scope": "domain",
                    "condition": { "field": "sender_domain", "op": "eq", "value": "n.example" },
                    "effect": { "move": "N" }
                }
            },
            {
                "proposal_type": "refine_rule",
                "risk_level": "low",
                "title": "refine",
                "rationale": "r",
                "recommended_status": "shadow_mode",
                "target_rule_kind": "action",
                "target_rule_id": target.as_str()
            }
        ],
        "rationale": "mixed"
    });
    let provider: Arc<dyn AiProvider> = Arc::new(MockProvider::returning_json("mock", value));
    let curator = curator(&repos, provider, vec![]);

    // A propose-only request drops the refine proposal.
    let report = block_on(curator.curate(CuratorRequest::just(CuratorOperation::Propose))).unwrap();
    assert_eq!(report.proposals.len(), 1);
    assert_eq!(report.proposals[0].proposal_type, ProposalKind::NewRule);
}
