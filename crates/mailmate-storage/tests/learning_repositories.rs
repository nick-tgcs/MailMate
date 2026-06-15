//! Integration tests for the Phase-7 learning repositories — rules (+ immutable versions),
//! audit timeline, the per-task feedback tables, and proposals (+ evidence links) — driven
//! through the public adapters over a migrated in-memory backend.

use std::sync::Arc;

use futures::executor::block_on;

use mailmate_common::actor::Actor;
use mailmate_common::audit::{event_type, AuditEntry, AuditQuery};
use mailmate_common::conflict::{ConflictStatus, RuleConflictRecord};
use mailmate_common::evidence::{EvidenceKind, EvidenceSourceKind, RuleEvidence};
use mailmate_common::features::{FeatureValue, FeatureVector};
use mailmate_common::feedback::{
    ClassificationFeedback, ClassificationFeedbackQuery, ClassificationFeedbackRow,
    FeedbackPolarity, FilingFeedback, FilingFeedbackQuery, FilingFeedbackRow, PinnedVersions,
    ProposalOutcome, RuleProposalFeedback, RuleProposalFeedbackQuery, RuleProposalFeedbackRow,
};
use mailmate_common::ids::{
    EvidenceId, FeedbackId, FolderId, MessageId, ProposalId, RuleId, RuleVersionId,
};
use mailmate_common::proposal::{AgentProposal, EvidenceRef, ProposalKind, ProposalStatus};
use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::evaluation::{ConflictKind, ConflictSeverity};
use mailmate_common::rules::rule::{
    HierarchyBand, NewRule, NewRuleVersion, RiskLevel, RuleKind, RuleScope, RuleStatus,
    RuleVersionContent,
};
use mailmate_common::shadow::ShadowOutcomeRow;
use mailmate_common::time::Timestamp;
use mailmate_ports::storage::audit::AuditRepository;
use mailmate_ports::storage::conflicts::ConflictRepository;
use mailmate_ports::storage::feedback::FeedbackRepository;
use mailmate_ports::storage::proposals::ProposalRepository;
use mailmate_ports::storage::rules::RuleRepository;
use mailmate_ports::storage::shadow_outcomes::ShadowOutcomeRepository;
use mailmate_storage::{
    open_and_migrate, SqliteAuditRepository, SqliteBackend, SqliteConflictRepository,
    SqliteFeedbackRepository, SqliteProposalRepository, SqliteRuleRepository,
    SqliteShadowOutcomeRepository, StorageConfig,
};

fn backend() -> Arc<SqliteBackend> {
    open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap()
}

fn version_content(folder: &str) -> RuleVersionContent {
    RuleVersionContent {
        title: "File receipts".to_owned(),
        description: "Move software receipts to a folder".to_owned(),
        condition: Condition::Predicate(Predicate {
            field: "sender_domain".to_owned(),
            op: Operator::Eq,
            value: FieldValue::Text("stripe.com".to_owned()),
        }),
        effect: RuleEffect {
            move_to: Some(folder.to_owned()),
            ..RuleEffect::new()
        },
        priority: 10,
        confidence_threshold: None,
        risk_level: RiskLevel::Low,
        change_reason: "initial".to_owned(),
        created_by: Actor::Ai,
    }
}

fn new_rule() -> NewRule {
    NewRule {
        stable_name: "file_stripe_receipts".to_owned(),
        kind: RuleKind::Action,
        scope: RuleScope::Domain,
        band: HierarchyBand::LearnedActive,
        created_by: Actor::Ai,
        initial_version: version_content("Receipts"),
    }
}

#[test]
fn rule_draft_versions_and_status_drive_the_active_snapshot() {
    let repo = SqliteRuleRepository::new(backend());

    // A fresh draft is not in the active snapshot (it is draft status).
    let rule_id = block_on(repo.save_rule_draft(new_rule())).unwrap();
    assert!(
        block_on(repo.get_active_rules(RuleKind::Action, RuleScope::Domain))
            .unwrap()
            .is_empty(),
        "a draft rule does not appear in the active snapshot"
    );

    // Activate it: now it loads, joined to version 1.
    block_on(repo.update_rule_status(&rule_id, RuleKind::Action, RuleStatus::Active)).unwrap();
    let active = block_on(repo.get_active_rules(RuleKind::Action, RuleScope::Domain)).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].rule_id, rule_id);
    assert_eq!(active[0].version.version_number, 1);
    assert_eq!(active[0].band, HierarchyBand::LearnedActive);
    assert_eq!(
        active[0].version.effect.move_to.as_deref(),
        Some("Receipts")
    );

    // Append version 2: the active snapshot now reflects the new current version.
    block_on(repo.create_rule_version(NewRuleVersion {
        rule_id: rule_id.clone(),
        kind: RuleKind::Action,
        content: version_content("Receipts/Software"),
    }))
    .unwrap();
    let active = block_on(repo.get_active_rules(RuleKind::Action, RuleScope::Domain)).unwrap();
    assert_eq!(
        active[0].version.version_number, 2,
        "monotonic version bump"
    );
    assert_eq!(
        active[0].version.effect.move_to.as_deref(),
        Some("Receipts/Software"),
        "current version re-points to v2"
    );

    // A shadow rule lives in a separate snapshot from the active one.
    block_on(repo.update_rule_status(&rule_id, RuleKind::Action, RuleStatus::ShadowMode)).unwrap();
    assert!(
        block_on(repo.get_active_rules(RuleKind::Action, RuleScope::Domain))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        block_on(repo.get_shadow_rules(RuleKind::Action, RuleScope::Domain))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn rule_kinds_use_separate_tables_and_stable_name_is_unique() {
    let repo = SqliteRuleRepository::new(backend());
    block_on(repo.save_rule_draft(new_rule())).unwrap();

    // The same stable_name in the SAME table is a constraint violation.
    let err = block_on(repo.save_rule_draft(new_rule())).unwrap_err();
    assert!(
        matches!(err, mailmate_common::error::StorageError::Constraint(_)),
        "duplicate stable_name must be rejected, got {err:?}"
    );

    // A classification rule with the same name is fine — it is a different table.
    let mut classification = new_rule();
    classification.kind = RuleKind::Classification;
    classification.scope = RuleScope::Global;
    let cls_id = block_on(repo.save_rule_draft(classification)).unwrap();
    block_on(repo.update_rule_status(&cls_id, RuleKind::Classification, RuleStatus::Active))
        .unwrap();
    assert_eq!(
        block_on(repo.get_active_rules(RuleKind::Classification, RuleScope::Global))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn audit_entries_append_and_query_newest_first() {
    let repo = SqliteAuditRepository::new(backend());
    let msg = MessageId::from("msg_audit");

    let id_first = block_on(repo.append(
        AuditEntry::new(event_type::ACTION_APPLIED, Actor::System).with_message(msg.clone()),
    ))
    .unwrap();
    // A distinct, later timestamp guarantees the DESC ordering is observable.
    let mut later = AuditEntry::new(event_type::RULE_PROPOSED, Actor::Ai).with_message(msg.clone());
    later.created_at = Timestamp::parse_rfc3339("2999-01-01T00:00:00Z").unwrap();
    let id_second = block_on(repo.append(later)).unwrap();

    let all = block_on(repo.query(AuditQuery {
        message_id: Some(msg.clone()),
        ..AuditQuery::default()
    }))
    .unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].id, id_second, "newest first");
    assert_eq!(all[1].id, id_first);

    // Filter by event_type narrows the result.
    let proposed = block_on(repo.query(AuditQuery {
        event_type: Some(event_type::RULE_PROPOSED.to_owned()),
        ..AuditQuery::default()
    }))
    .unwrap();
    assert_eq!(proposed.len(), 1);
    assert_eq!(proposed[0].event_type, "rule_proposed");
    assert_eq!(proposed[0].actor, Actor::Ai);
}

#[test]
fn classification_feedback_appends_and_filters_by_label() {
    let repo: SqliteFeedbackRepository = SqliteFeedbackRepository::new(backend());
    let mut salient = FeatureVector::new();
    salient.insert("sender_domain", FeatureValue::Text("paypa1.com".to_owned()));
    let row = ClassificationFeedbackRow {
        id: ClassificationFeedback::fresh_id(),
        message_id: MessageId::from("msg_c1"),
        pinned_versions: PinnedVersions {
            calibration_version: Some("logreg-identity-v1".to_owned()),
            ..PinnedVersions::default()
        },
        ai_label: Some("not_junk".to_owned()),
        ai_score: Some(0.1),
        ai_rationale: None,
        human_label: "phishing".to_owned(),
        human_reason_code: Some("spoofed_sender".to_owned()),
        human_reason_text: None,
        salient_features: salient,
        polarity: FeedbackPolarity::Negative,
        created_at: Timestamp::now(),
    };
    let id = block_on(FeedbackRepository::<ClassificationFeedback>::append(
        &repo, row,
    ))
    .unwrap();
    assert!(id.as_str().starts_with("clsfb_"));

    let phishing = block_on(FeedbackRepository::<ClassificationFeedback>::query(
        &repo,
        ClassificationFeedbackQuery {
            human_label: Some("phishing".to_owned()),
            ..ClassificationFeedbackQuery::default()
        },
    ))
    .unwrap();
    assert_eq!(phishing.len(), 1);
    assert_eq!(phishing[0].id, id);
    assert_eq!(
        phishing[0].salient_features.get("sender_domain"),
        Some(&FeatureValue::Text("paypa1.com".to_owned())),
        "the salient features round-trip through JSON"
    );
    assert_eq!(
        phishing[0].pinned_versions.calibration_version.as_deref(),
        Some("logreg-identity-v1")
    );

    // A different label is filtered out.
    assert!(
        block_on(FeedbackRepository::<ClassificationFeedback>::query(
            &repo,
            ClassificationFeedbackQuery {
                human_label: Some("spam".to_owned()),
                ..ClassificationFeedbackQuery::default()
            },
        ))
        .unwrap()
        .is_empty()
    );
}

#[test]
fn filing_feedback_appends_and_filters_by_folder() {
    let repo: SqliteFeedbackRepository = SqliteFeedbackRepository::new(backend());
    let row = FilingFeedbackRow {
        id: FilingFeedback::fresh_id(),
        message_id: MessageId::from("msg_f1"),
        pinned_versions: PinnedVersions::default(),
        sender_domain: Some("stripe.com".to_owned()),
        ai_suggested_folder: None,
        human_chosen_folder: FolderId::from("folder_receipts"),
        basis: Some("domain".to_owned()),
        matched_rule_id: None,
        polarity: FeedbackPolarity::Negative,
        created_at: Timestamp::now(),
    };
    let id = block_on(FeedbackRepository::<FilingFeedback>::append(&repo, row)).unwrap();
    assert!(id.as_str().starts_with("filfb_"));

    let receipts = block_on(FeedbackRepository::<FilingFeedback>::query(
        &repo,
        FilingFeedbackQuery {
            human_chosen_folder: Some(FolderId::from("folder_receipts")),
            ..FilingFeedbackQuery::default()
        },
    ))
    .unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].sender_domain.as_deref(), Some("stripe.com"));
    assert_eq!(receipts[0].basis.as_deref(), Some("domain"));
}

fn evidence(source_id: &str) -> RuleEvidence {
    RuleEvidence {
        id: EvidenceId::fresh(),
        rule_kind: None,
        rule_id: None,
        proposal_id: None,
        source_kind: EvidenceSourceKind::Filing,
        source_id: FeedbackId::from(source_id),
        message_id: Some(MessageId::from("msg_e")),
        evidence_kind: EvidenceKind::Negative,
        weight: 1.0,
        summary: "user moved to Receipts".to_owned(),
        created_at: Timestamp::now(),
    }
}

fn proposal() -> AgentProposal {
    AgentProposal {
        id: ProposalId::fresh(),
        proposal_type: ProposalKind::NewRule,
        status: ProposalStatus::PendingReview,
        title: "File Stripe receipts".to_owned(),
        rationale: "3 moves to Receipts from stripe.com".to_owned(),
        risk_level: RiskLevel::Low,
        recommended_status: RuleStatus::ShadowMode,
        rule_draft: None,
        target_rule_kind: None,
        target_rule_id: None,
        evidence_refs: vec![EvidenceRef {
            kind: EvidenceSourceKind::Filing,
            id: FeedbackId::from("filfb_1"),
        }],
        source_provider: "learning-engine".to_owned(),
        created_at: Timestamp::now(),
        reviewed_at: None,
    }
}

#[test]
fn proposal_saves_with_evidence_and_drives_review_status() {
    let repo = SqliteProposalRepository::new(backend());
    let prop = proposal();
    let prop_id = prop.id.clone();
    block_on(repo.save(prop, vec![evidence("filfb_1"), evidence("filfb_2")])).unwrap();

    // It is found by its pending status, and the evidence links carry the proposal id.
    let pending = block_on(repo.list_by_status(ProposalStatus::PendingReview)).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, prop_id);
    let links = block_on(repo.evidence_for(&prop_id)).unwrap();
    assert_eq!(links.len(), 2);
    assert!(links
        .iter()
        .all(|e| e.proposal_id.as_ref() == Some(&prop_id)));

    // Transition to accepted with a review time; the column is authoritative on read-back.
    let reviewed = Timestamp::now();
    block_on(repo.set_status(&prop_id, ProposalStatus::Accepted, Some(reviewed))).unwrap();
    let got = block_on(repo.get(&prop_id)).unwrap().unwrap();
    assert_eq!(got.status, ProposalStatus::Accepted);
    assert!(got.reviewed_at.is_some(), "review time recorded");
    assert!(
        block_on(repo.list_by_status(ProposalStatus::PendingReview))
            .unwrap()
            .is_empty(),
        "no longer pending"
    );

    assert!(block_on(repo.get(&ProposalId::from("prop_absent")))
        .unwrap()
        .is_none());
}

#[test]
fn shadow_outcomes_append_and_read_back_per_rule() {
    let repo = SqliteShadowOutcomeRepository::new(backend());
    let rule_id = RuleId::from("rule_shadowed");
    let hit = ShadowOutcomeRow::new(
        RuleKind::Action,
        rule_id.clone(),
        RuleVersionId::from("rv_1"),
        MessageId::from("msg_s1"),
        RuleEffect {
            move_to: Some("Receipts".to_owned()),
            ..RuleEffect::new()
        },
        "requires_review",
    )
    .with_match(true);
    let miss = ShadowOutcomeRow::new(
        RuleKind::Action,
        rule_id.clone(),
        RuleVersionId::from("rv_1"),
        MessageId::from("msg_s2"),
        RuleEffect {
            move_to: Some("Receipts".to_owned()),
            ..RuleEffect::new()
        },
        "requires_review",
    )
    .with_match(false);
    block_on(repo.append(hit)).unwrap();
    block_on(repo.append(miss)).unwrap();
    // A firing for a different rule is not returned for this one.
    block_on(repo.append(ShadowOutcomeRow::new(
        RuleKind::Action,
        RuleId::from("rule_other"),
        RuleVersionId::from("rv_2"),
        MessageId::from("msg_s3"),
        RuleEffect::new(),
        "allowed",
    )))
    .unwrap();

    let rows = block_on(repo.list_for_rule(&rule_id)).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .filter(|r| r.matched_later_user_action == Some(true))
            .count(),
        1
    );
    assert_eq!(
        rows[0].would_have_action.move_to.as_deref(),
        Some("Receipts"),
        "the would-have effect round-trips through JSON"
    );
}

#[test]
fn conflicts_append_list_open_and_resolve() {
    let repo = SqliteConflictRepository::new(backend());
    let conflict = RuleConflictRecord::new(
        RuleKind::Action,
        RuleId::from("rule_a"),
        RuleId::from("rule_b"),
        ConflictKind::ContradictoryEffect,
        ConflictSeverity::High,
        "both file stripe.com to different folders",
    );
    let conflict_id = block_on(repo.append(conflict)).unwrap();
    assert!(conflict_id.as_str().starts_with("conf_"));

    // It shows up in the open queue, with its fields round-tripped.
    let open = block_on(repo.list_open()).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, conflict_id);
    assert_eq!(open[0].rule_a_id, RuleId::from("rule_a"));
    assert_eq!(open[0].conflict_kind, ConflictKind::ContradictoryEffect);
    assert_eq!(open[0].severity, ConflictSeverity::High);
    assert_eq!(open[0].status, ConflictStatus::Open);
    assert!(open[0].resolved_at.is_none());

    // Resolving it removes it from the open queue and records the resolution time.
    let resolved = Timestamp::now();
    block_on(repo.set_status(&conflict_id, ConflictStatus::Resolved, Some(resolved))).unwrap();
    assert!(
        block_on(repo.list_open()).unwrap().is_empty(),
        "a resolved conflict leaves the open queue"
    );

    // A second conflict can be *ignored* with no resolution time (the None branch), and it
    // likewise leaves the open queue.
    let other = block_on(repo.append(RuleConflictRecord::new(
        RuleKind::Classification,
        RuleId::from("rule_c"),
        RuleId::from("rule_d"),
        ConflictKind::ContradictoryEffect,
        ConflictSeverity::Medium,
        "two labels for the same domain",
    )))
    .unwrap();
    assert_eq!(block_on(repo.list_open()).unwrap().len(), 1);
    block_on(repo.set_status(&other, ConflictStatus::Ignored, None)).unwrap();
    assert!(block_on(repo.list_open()).unwrap().is_empty());
}

#[test]
fn rule_proposal_feedback_appends_and_filters_by_proposal() {
    let repo: SqliteFeedbackRepository = SqliteFeedbackRepository::new(backend());
    let proposal_id = ProposalId::from("prop_reviewed");
    let row = RuleProposalFeedbackRow {
        id: RuleProposalFeedback::fresh_id(),
        proposal_id: proposal_id.clone(),
        pinned_versions: PinnedVersions {
            prompt_template_version: Some("curate-v1".to_owned()),
            ..PinnedVersions::default()
        },
        outcome: ProposalOutcome::Accepted,
        human_reason_code: Some("matches_my_filing".to_owned()),
        human_reason_text: None,
        polarity: FeedbackPolarity::Positive,
        created_at: Timestamp::now(),
    };
    let id = block_on(FeedbackRepository::<RuleProposalFeedback>::append(
        &repo, row,
    ))
    .unwrap();
    assert!(id.as_str().starts_with("rpffb_"));

    let for_proposal = block_on(FeedbackRepository::<RuleProposalFeedback>::query(
        &repo,
        RuleProposalFeedbackQuery {
            proposal_id: Some(proposal_id.clone()),
            ..RuleProposalFeedbackQuery::default()
        },
    ))
    .unwrap();
    assert_eq!(for_proposal.len(), 1);
    assert_eq!(for_proposal[0].id, id);
    assert_eq!(for_proposal[0].outcome, ProposalOutcome::Accepted);
    assert_eq!(
        for_proposal[0]
            .pinned_versions
            .prompt_template_version
            .as_deref(),
        Some("curate-v1")
    );

    // A different proposal id filters this row out.
    assert!(block_on(FeedbackRepository::<RuleProposalFeedback>::query(
        &repo,
        RuleProposalFeedbackQuery {
            proposal_id: Some(ProposalId::from("prop_other")),
            ..RuleProposalFeedbackQuery::default()
        },
    ))
    .unwrap()
    .is_empty());
}
