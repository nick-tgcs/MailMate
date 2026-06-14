//! Integration tests for the learning loop end to end: the [`DefaultLearningEngine`] over
//! the **real** SQLite repositories. Capture → evidence → proposal → audit, the threshold
//! gate, the crystallization back-test, and outcome monitoring — no backend mock, no GPU.

use std::sync::Arc;

use futures::executor::block_on;

use mailmate_common::actor::Actor;
use mailmate_common::audit::{event_type, AuditQuery};
use mailmate_common::evidence::{EvidenceQuery, EvidenceSourceKind};
use mailmate_common::features::{FeatureValue, FeatureVector};
use mailmate_common::feedback::{
    ClassificationFeedback, ClassificationFeedbackRow, FeedbackPolarity, FilingFeedback,
    FilingFeedbackRow, PinnedVersions, TaskFeedback,
};
use mailmate_common::ids::{FolderId, MessageId, RuleId, RuleVersionId};
use mailmate_common::outcome::RuleOutcome;
use mailmate_common::proposal::{ProposalStatus, ProposalTrigger};
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::rule::RuleKind;
use mailmate_common::shadow::ShadowOutcomeRow;
use mailmate_common::time::Timestamp;
use mailmate_learning::outcomes::aggregate_rule_outcome;
use mailmate_learning::shadow::{back_test, HistoricalExample};
use mailmate_learning::DefaultLearningEngine;
use mailmate_ports::learning_engine::LearningEngine;
use mailmate_ports::storage::audit::AuditRepository;
use mailmate_ports::storage::feedback::FeedbackRepository;
use mailmate_ports::storage::proposals::ProposalRepository;
use mailmate_ports::storage::shadow_outcomes::ShadowOutcomeRepository;
use mailmate_storage::{
    open_and_migrate, SqliteAuditRepository, SqliteFeedbackRepository, SqliteProposalRepository,
    SqliteShadowOutcomeRepository, StorageConfig,
};

/// A wired engine plus the audit/proposal repos for assertions.
struct Harness {
    engine: DefaultLearningEngine,
    audit: Arc<SqliteAuditRepository>,
    proposals: Arc<SqliteProposalRepository>,
}

fn harness() -> Harness {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let feedback = Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    let classification: Arc<dyn FeedbackRepository<ClassificationFeedback>> = feedback.clone();
    let filing: Arc<dyn FeedbackRepository<FilingFeedback>> = feedback.clone();
    let audit = Arc::new(SqliteAuditRepository::new(backend.clone()));
    let proposals = Arc::new(SqliteProposalRepository::new(backend.clone()));
    let engine =
        DefaultLearningEngine::new(classification, filing, audit.clone(), proposals.clone());
    Harness {
        engine,
        audit,
        proposals,
    }
}

fn filing_row(domain: &str, folder: &str) -> FilingFeedbackRow {
    FilingFeedbackRow {
        id: FilingFeedback::fresh_id(),
        message_id: MessageId::fresh(),
        pinned_versions: PinnedVersions::default(),
        sender_domain: Some(domain.to_owned()),
        ai_suggested_folder: None,
        human_chosen_folder: FolderId::from(folder),
        basis: Some("domain".to_owned()),
        matched_rule_id: None,
        polarity: FeedbackPolarity::Negative,
        created_at: Timestamp::now(),
    }
}

fn classification_row(domain: &str, label: &str) -> ClassificationFeedbackRow {
    let mut salient = FeatureVector::new();
    salient.insert("sender_domain", FeatureValue::Text(domain.to_owned()));
    ClassificationFeedbackRow {
        id: ClassificationFeedback::fresh_id(),
        message_id: MessageId::fresh(),
        pinned_versions: PinnedVersions::default(),
        ai_label: Some("not_junk".to_owned()),
        ai_score: Some(0.1),
        ai_rationale: None,
        human_label: label.to_owned(),
        human_reason_code: Some("spoofed_sender".to_owned()),
        human_reason_text: None,
        salient_features: salient,
        polarity: FeedbackPolarity::Negative,
        created_at: Timestamp::now(),
    }
}

#[test]
fn capture_routes_each_correction_to_its_owner_and_derives_evidence() {
    let h = harness();
    block_on(
        h.engine
            .record_feedback(TaskFeedback::Filing(filing_row("stripe.com", "Receipts"))),
    )
    .unwrap();
    block_on(
        h.engine
            .record_feedback(TaskFeedback::Classification(classification_row(
                "paypa1.com",
                "phishing",
            ))),
    )
    .unwrap();

    // collect_evidence derives one item per captured row, tagged by source.
    let all = block_on(h.engine.collect_evidence(EvidenceQuery::all())).unwrap();
    assert_eq!(all.len(), 2);
    assert!(all
        .iter()
        .any(|e| e.source_kind == EvidenceSourceKind::Filing));
    assert!(all
        .iter()
        .any(|e| e.source_kind == EvidenceSourceKind::Classification));

    // A source filter narrows it to one table.
    let filing_only = block_on(
        h.engine
            .collect_evidence(EvidenceQuery::for_source(EvidenceSourceKind::Filing)),
    )
    .unwrap();
    assert_eq!(filing_only.len(), 1);
    assert_eq!(filing_only[0].source_kind, EvidenceSourceKind::Filing);
}

#[test]
fn three_same_domain_moves_cross_the_threshold_and_emit_a_reviewable_proposal() {
    let h = harness();
    for _ in 0..3 {
        block_on(
            h.engine
                .record_feedback(TaskFeedback::Filing(filing_row("stripe.com", "Receipts"))),
        )
        .unwrap();
    }
    // A different domain with only two moves stays below the bar.
    for _ in 0..2 {
        block_on(
            h.engine
                .record_feedback(TaskFeedback::Filing(filing_row("github.com", "Code"))),
        )
        .unwrap();
    }

    let proposals = block_on(h.engine.propose_candidates(ProposalTrigger::all())).unwrap();
    assert_eq!(
        proposals.len(),
        1,
        "only the 3-move cluster crosses the bar"
    );
    let proposal = &proposals[0];
    assert_eq!(proposal.status, ProposalStatus::PendingReview);
    let draft = proposal.rule_draft.as_ref().unwrap();
    assert_eq!(draft.effect.move_to.as_deref(), Some("Receipts"));

    // It is persisted, queryable by status, with its three evidence links.
    let pending = block_on(h.proposals.list_by_status(ProposalStatus::PendingReview)).unwrap();
    assert_eq!(pending.len(), 1);
    let links = block_on(h.proposals.evidence_for(&proposal.id)).unwrap();
    assert_eq!(links.len(), 3);
    assert!(links
        .iter()
        .all(|e| e.proposal_id.as_ref() == Some(&proposal.id)));

    // Emitting a proposal writes a `rule_proposed` audit entry naming it.
    let audited = block_on(h.audit.query(AuditQuery {
        event_type: Some(event_type::RULE_PROPOSED.to_owned()),
        ..AuditQuery::default()
    }))
    .unwrap();
    assert_eq!(audited.len(), 1);
    assert_eq!(audited[0].proposal_id.as_ref(), Some(&proposal.id));
    assert_eq!(audited[0].actor, Actor::Ai);
}

#[test]
fn repeated_phishing_corrections_emit_a_classification_proposal() {
    let h = harness();
    for _ in 0..2 {
        block_on(
            h.engine
                .record_feedback(TaskFeedback::Classification(classification_row(
                    "paypa1.com",
                    "phishing",
                ))),
        )
        .unwrap();
    }

    let proposals = block_on(h.engine.propose_candidates(ProposalTrigger {
        source_kind: Some(EvidenceSourceKind::Classification),
        ..ProposalTrigger::all()
    }))
    .unwrap();
    assert_eq!(proposals.len(), 1);
    let draft = proposals[0].rule_draft.as_ref().unwrap();
    assert_eq!(draft.kind, RuleKind::Classification);
    assert_eq!(draft.effect.set_labels, vec!["phishing".to_owned()]);
}

#[test]
fn a_proposed_candidate_crystallizes_only_when_it_clears_the_history_bar() {
    let h = harness();
    for _ in 0..3 {
        block_on(
            h.engine
                .record_feedback(TaskFeedback::Filing(filing_row("stripe.com", "Receipts"))),
        )
        .unwrap();
    }
    let proposals = block_on(h.engine.propose_candidates(ProposalTrigger::all())).unwrap();
    let draft = proposals[0].rule_draft.clone().unwrap();

    // Back-test the candidate against a clean history: it reproduces every decision.
    let clean: Vec<HistoricalExample> = (0..3)
        .map(|_| {
            HistoricalExample::from_domain(
                "stripe.com",
                RuleEffect {
                    move_to: Some("Receipts".to_owned()),
                    ..RuleEffect::new()
                },
            )
        })
        .collect();
    let report = block_on(back_test(&draft, &clean)).unwrap();
    assert!(report.is_eligible(0.9, 3), "clean history → eligible");

    // The same candidate against a contradictory history is NOT eligible.
    let messy = vec![
        HistoricalExample::from_domain(
            "stripe.com",
            RuleEffect {
                move_to: Some("Personal".to_owned()),
                ..RuleEffect::new()
            },
        ),
        HistoricalExample::from_domain(
            "stripe.com",
            RuleEffect {
                move_to: Some("Personal".to_owned()),
                ..RuleEffect::new()
            },
        ),
        HistoricalExample::from_domain(
            "stripe.com",
            RuleEffect {
                move_to: Some("Receipts".to_owned()),
                ..RuleEffect::new()
            },
        ),
    ];
    let report = block_on(back_test(&draft, &messy)).unwrap();
    assert!(
        !report.is_eligible(0.9, 3),
        "contradictory history → blocked"
    );
}

#[test]
fn outcome_monitoring_derives_live_and_shadow_precision_from_real_rows() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let feedback = SqliteFeedbackRepository::new(backend.clone());
    let shadow = SqliteShadowOutcomeRepository::new(backend.clone());
    let rule_id = RuleId::from("rule_receipts");

    // Two agreed filing rows and one override, all naming the rule.
    for polarity in [
        FeedbackPolarity::Positive,
        FeedbackPolarity::Positive,
        FeedbackPolarity::Negative,
    ] {
        let mut row = filing_row("stripe.com", "Receipts");
        row.matched_rule_id = Some(rule_id.clone());
        row.polarity = polarity;
        block_on(FeedbackRepository::<FilingFeedback>::append(&feedback, row)).unwrap();
    }
    // Two shadow firings, one matched by the user later.
    for matched in [Some(true), Some(false)] {
        let mut row = ShadowOutcomeRow::new(
            RuleKind::Action,
            rule_id.clone(),
            RuleVersionId::from("rv_1"),
            MessageId::fresh(),
            RuleEffect {
                move_to: Some("Receipts".to_owned()),
                ..RuleEffect::new()
            },
            "requires_review",
        );
        row.matched_later_user_action = matched;
        block_on(shadow.append(row)).unwrap();
    }

    let filing_rows = block_on(FeedbackRepository::<FilingFeedback>::query(
        &feedback,
        mailmate_common::feedback::FilingFeedbackQuery::default(),
    ))
    .unwrap();
    let shadow_rows = block_on(shadow.list_for_rule(&rule_id)).unwrap();
    let outcome: RuleOutcome =
        aggregate_rule_outcome(&rule_id, RuleKind::Action, &filing_rows, &shadow_rows);

    assert_eq!(outcome.fire_count, 3);
    assert_eq!(outcome.precision(), Some(2.0 / 3.0));
    assert_eq!(outcome.shadow_total, 2);
    assert_eq!(outcome.shadow_matched, 1);
    assert_eq!(outcome.shadow_precision(), Some(0.5));
}
