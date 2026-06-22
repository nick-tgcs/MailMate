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
    SqliteRuleRepository, SqliteShadowOutcomeRepository, StorageConfig,
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

    // Phase 6: the gate's back-test is captured ON the proposal (precision · support) so the
    // Review card shows the exact numbers the candidate was admitted on — not a re-derivation.
    let bt = proposal.back_test.expect("a filing proposal carries its back-test");
    assert_eq!(bt.support, 3, "fired on the three historical moves");
    assert_eq!(bt.precision, Some(1.0), "all three agreed → precision 1.0");

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

/// A classification correction carrying an arbitrary captured feature set (not just a domain) —
/// the rich-capture rows multi-aspect induction mines.
fn classification_row_with(label: &str, features: &[(&str, FeatureValue)]) -> ClassificationFeedbackRow {
    let mut salient = FeatureVector::new();
    for (k, v) in features {
        salient.insert(*k, v.clone());
    }
    ClassificationFeedbackRow {
        id: ClassificationFeedback::fresh_id(),
        message_id: MessageId::fresh(),
        pinned_versions: PinnedVersions::default(),
        ai_label: Some("not_junk".to_owned()),
        ai_score: None,
        ai_rationale: None,
        human_label: label.to_owned(),
        human_reason_code: None,
        human_reason_text: None,
        salient_features: salient,
        polarity: FeedbackPolarity::Negative,
        created_at: Timestamp::now(),
    }
}

/// Save + activate an action rule, returning its id.
fn seed_active_rule(rules: &SqliteRuleRepository, stable_name: &str, domain: &str) -> RuleId {
    use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
    use mailmate_common::rules::rule::{
        HierarchyBand, NewRule, RiskLevel, RuleScope, RuleStatus, RuleVersionContent,
    };
    use mailmate_ports::storage::rules::RuleRepository;

    let rule_id = block_on(rules.save_rule_draft(NewRule {
        stable_name: stable_name.to_owned(),
        kind: RuleKind::Action,
        scope: RuleScope::Domain,
        band: HierarchyBand::LearnedActive,
        created_by: Actor::Ai,
        initial_version: RuleVersionContent {
            title: "File mail".to_owned(),
            description: "move by sender domain".to_owned(),
            condition: Condition::Predicate(Predicate {
                field: "sender_domain".to_owned(),
                op: Operator::Eq,
                value: FieldValue::Text(domain.to_owned()),
            }),
            effect: RuleEffect {
                move_to: Some("Spam".to_owned()),
                ..RuleEffect::new()
            },
            priority: 10,
            confidence_threshold: None,
            risk_level: RiskLevel::Low,
            change_reason: "test".to_owned(),
            created_by: Actor::Ai,
        },
    }))
    .unwrap();
    block_on(rules.update_rule_status(&rule_id, RuleKind::Action, RuleStatus::Active)).unwrap();
    rule_id
}

#[test]
fn a_rule_the_user_keeps_undoing_surfaces_a_human_gated_retire_proposal() {
    // Phase 7 EXIT #2, end to end: an active rule whose auto-applied action the user keeps undoing
    // decays. The per-rule `action_undone` provenance crosses the count bar, and the engine
    // surfaces a RetireRule proposal — for a human to decide. It NEVER retires the rule itself.
    use mailmate_common::proposal::ProposalKind;
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let feedback = Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    let classification: Arc<dyn FeedbackRepository<ClassificationFeedback>> = feedback.clone();
    let filing: Arc<dyn FeedbackRepository<FilingFeedback>> = feedback.clone();
    let audit = Arc::new(SqliteAuditRepository::new(backend.clone()));
    let proposals = Arc::new(SqliteProposalRepository::new(backend.clone()));
    let rules = Arc::new(SqliteRuleRepository::new(backend.clone()));
    let engine = DefaultLearningEngine::new(classification, filing, audit.clone(), proposals.clone())
        .with_rules(rules.clone());

    let rule_id = seed_active_rule(&rules, "keep-undoing", "noisy.example");
    let other_id = seed_active_rule(&rules, "well-behaved", "clean.example");

    // The user undoes the first rule's action three times — three stamped `action_undone` rows.
    for _ in 0..3 {
        let entry = mailmate_common::audit::AuditEntry::new(event_type::ACTION_UNDONE, Actor::User)
            .with_rule(RuleKind::Action, rule_id.clone());
        block_on(audit.append(entry)).unwrap();
    }

    let emitted = block_on(engine.propose_candidates(ProposalTrigger::all())).unwrap();
    let retire = emitted
        .iter()
        .find(|p| p.proposal_type == ProposalKind::RetireRule)
        .expect("a high-undo rule surfaces a retire proposal");
    assert_eq!(retire.target_rule_id.as_ref(), Some(&rule_id));
    assert_eq!(retire.target_rule_kind, Some(RuleKind::Action));
    assert_eq!(retire.status, ProposalStatus::PendingReview, "human-gated");
    assert!(retire.rule_draft.is_none(), "a retire references the rule, no new draft");
    // The well-behaved rule (zero undos) is NOT proposed for retirement.
    assert!(
        !emitted.iter().any(|p| p.proposal_type == ProposalKind::RetireRule
            && p.target_rule_id.as_ref() == Some(&other_id)),
        "a rule with no undos is left alone"
    );

    // The rule stays ACTIVE — the engine proposed, it did not retire.
    use mailmate_ports::storage::rules::RuleRepository;
    let active = block_on(rules.get_active_rules(
        RuleKind::Action,
        mailmate_common::rules::rule::RuleScope::Domain,
    ))
    .unwrap();
    assert!(
        active.iter().any(|r| r.rule_id == rule_id),
        "the engine surfaced a proposal but never retired the rule itself"
    );

    // Idempotent: a second pass does not re-propose the same retirement.
    let again = block_on(engine.propose_candidates(ProposalTrigger::all())).unwrap();
    assert!(
        !again.iter().any(|p| p.proposal_type == ProposalKind::RetireRule),
        "a retirement proposes once, not once per pass: {again:?}"
    );
}

#[test]
fn with_real_per_rule_fires_a_high_volume_rule_is_protected_by_the_undo_rate() {
    // The keystone's payoff: now that `action_applied` is stamped with the authoring rule_id, the
    // decay pass has a real per-rule fires denominator and the undo-*rate* governs — so a busy,
    // mostly-good rule (undone 4 of 20 fires = 20%) is NOT retired, even though its raw undo COUNT
    // (4) crosses the count-only bar the old code would have retired it on. No fabricated rate.
    use mailmate_common::proposal::ProposalKind;
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let feedback = Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    let classification: Arc<dyn FeedbackRepository<ClassificationFeedback>> = feedback.clone();
    let filing: Arc<dyn FeedbackRepository<FilingFeedback>> = feedback.clone();
    let audit = Arc::new(SqliteAuditRepository::new(backend.clone()));
    let proposals = Arc::new(SqliteProposalRepository::new(backend.clone()));
    let rules = Arc::new(SqliteRuleRepository::new(backend.clone()));
    let engine = DefaultLearningEngine::new(classification, filing, audit.clone(), proposals.clone())
        .with_rules(rules.clone());

    let rule_id = seed_active_rule(&rules, "busy-but-good", "busy.example");
    // 20 real fires (per-rule `action_applied`, the new denominator) and 4 undos → a 20% rate.
    for _ in 0..20 {
        let fire = mailmate_common::audit::AuditEntry::new(event_type::ACTION_APPLIED, Actor::System)
            .with_rule(RuleKind::Action, rule_id.clone());
        block_on(audit.append(fire)).unwrap();
    }
    for _ in 0..4 {
        let undo = mailmate_common::audit::AuditEntry::new(event_type::ACTION_UNDONE, Actor::User)
            .with_rule(RuleKind::Action, rule_id.clone());
        block_on(audit.append(undo)).unwrap();
    }

    let emitted = block_on(engine.propose_candidates(ProposalTrigger::all())).unwrap();
    assert!(
        !emitted.iter().any(|p| p.proposal_type == ProposalKind::RetireRule
            && p.target_rule_id.as_ref() == Some(&rule_id)),
        "a 20%-undo-rate rule over 20 real fires is good — the rate governs, the raw count does not"
    );
}

#[test]
fn a_misbehaving_rule_retires_on_the_real_undo_rate_with_a_rate_rationale() {
    // The other side of the keystone: with real fires, a rule undone 4 of 10 times (40%) crosses
    // the rate floor and the retire card states the REAL numbers — fired/undone/rate, not a count.
    use mailmate_common::proposal::ProposalKind;
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let feedback = Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    let classification: Arc<dyn FeedbackRepository<ClassificationFeedback>> = feedback.clone();
    let filing: Arc<dyn FeedbackRepository<FilingFeedback>> = feedback.clone();
    let audit = Arc::new(SqliteAuditRepository::new(backend.clone()));
    let proposals = Arc::new(SqliteProposalRepository::new(backend.clone()));
    let rules = Arc::new(SqliteRuleRepository::new(backend.clone()));
    let engine = DefaultLearningEngine::new(classification, filing, audit.clone(), proposals.clone())
        .with_rules(rules.clone());

    let rule_id = seed_active_rule(&rules, "really-bad", "bad.example");
    for _ in 0..10 {
        let fire = mailmate_common::audit::AuditEntry::new(event_type::ACTION_APPLIED, Actor::System)
            .with_rule(RuleKind::Action, rule_id.clone());
        block_on(audit.append(fire)).unwrap();
    }
    for _ in 0..4 {
        let undo = mailmate_common::audit::AuditEntry::new(event_type::ACTION_UNDONE, Actor::User)
            .with_rule(RuleKind::Action, rule_id.clone());
        block_on(audit.append(undo)).unwrap();
    }

    let emitted = block_on(engine.propose_candidates(ProposalTrigger::all())).unwrap();
    let retire = emitted
        .iter()
        .find(|p| p.proposal_type == ProposalKind::RetireRule
            && p.target_rule_id.as_ref() == Some(&rule_id))
        .expect("a 40%-undo-rate rule over 10 real fires crosses the rate floor");
    assert!(
        retire.rationale.contains("fired 10 times") && retire.rationale.contains("40% undo rate"),
        "the card states the real rate, not a bare count: {}",
        retire.rationale
    );
}

#[test]
fn repeated_outbound_mail_surfaces_a_human_gated_vip_priority_proposal() {
    // Phase-7 learn-from-Sent: the user emails a domain repeatedly; the engine mines the outbound
    // `mail_sent` audit trail and surfaces a human-gated VIP/priority rule for that domain — a
    // deterministic `sender_domain == domain → priority high` candidate, recommended shadow, never
    // auto-activated. A domain emailed only once stays below the bar.
    use mailmate_common::proposal::ProposalKind;
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let feedback = Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    let classification: Arc<dyn FeedbackRepository<ClassificationFeedback>> = feedback.clone();
    let filing: Arc<dyn FeedbackRepository<FilingFeedback>> = feedback.clone();
    let audit = Arc::new(SqliteAuditRepository::new(backend.clone()));
    let proposals = Arc::new(SqliteProposalRepository::new(backend.clone()));
    let engine =
        DefaultLearningEngine::new(classification, filing, audit.clone(), proposals.clone());

    // Three sends to acme.com (over the default bar of 3) and one to rare.com (below it).
    for _ in 0..3 {
        let sent = mailmate_common::audit::AuditEntry::new(event_type::MAIL_SENT, Actor::User)
            .with_payload(serde_json::json!({ "recipient_domain": "acme.com" }));
        block_on(audit.append(sent)).unwrap();
    }
    let once = mailmate_common::audit::AuditEntry::new(event_type::MAIL_SENT, Actor::User)
        .with_payload(serde_json::json!({ "recipient_domain": "rare.com" }));
    block_on(audit.append(once)).unwrap();

    let emitted = block_on(engine.propose_candidates(ProposalTrigger::all())).unwrap();
    let vip = emitted
        .iter()
        .find(|p| p.proposal_type == ProposalKind::NewRule && p.title.contains("acme.com"))
        .expect("a frequently-emailed domain surfaces a VIP proposal");
    assert_eq!(vip.status, ProposalStatus::PendingReview, "human-gated");
    assert_eq!(vip.recommended_status.as_str(), "shadow_mode", "never auto-activated");
    let draft = vip.rule_draft.as_ref().unwrap();
    assert_eq!(draft.kind, RuleKind::Classification);
    assert_eq!(draft.effect.priority.as_deref(), Some("high"));
    assert!(vip.rationale.contains("3 times"), "{}", vip.rationale);
    // The once-emailed domain is below the bar — no VIP proposal.
    assert!(
        !emitted.iter().any(|p| p.title.contains("rare.com")),
        "one send is not enough to propose a VIP rule"
    );

    // Idempotent: a second pass does not re-propose the same VIP rule.
    let again = block_on(engine.propose_candidates(ProposalTrigger::all())).unwrap();
    assert!(
        !again.iter().any(|p| p.title.contains("acme.com")),
        "a VIP rule proposes once, not once per pass"
    );
}

#[test]
fn a_rule_that_has_gone_quiet_surfaces_a_human_gated_stale_retire_proposal() {
    // Phase-7 7b DetectStale: a live rule that fired in the past but has not fired in the idle
    // window is dead weight. With a clock, the decay pass surfaces a human-gated retire whose card
    // states the idle signal — and, like every decay proposal, never retires the rule itself.
    use mailmate_common::proposal::ProposalKind;
    use mailmate_test_support::fakes::FakeClock;
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let feedback = Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    let classification: Arc<dyn FeedbackRepository<ClassificationFeedback>> = feedback.clone();
    let filing: Arc<dyn FeedbackRepository<FilingFeedback>> = feedback.clone();
    let audit = Arc::new(SqliteAuditRepository::new(backend.clone()));
    let proposals = Arc::new(SqliteProposalRepository::new(backend.clone()));
    let rules = Arc::new(SqliteRuleRepository::new(backend.clone()));

    let quiet_id = seed_active_rule(&rules, "went-quiet", "quiet.example");
    let busy_id = seed_active_rule(&rules, "still-busy", "busy.example");

    // The quiet rule's only fire is 90 days old; the busy rule fired just now. The audit store
    // preserves each row's `created_at`, so we backdate the quiet fire to drive real idleness.
    let now = Timestamp::now();
    let mut quiet_fire =
        mailmate_common::audit::AuditEntry::new(event_type::ACTION_APPLIED, Actor::System)
            .with_rule(RuleKind::Action, quiet_id.clone());
    quiet_fire.created_at = now.add_days(-90);
    block_on(audit.append(quiet_fire)).unwrap();
    let busy_fire =
        mailmate_common::audit::AuditEntry::new(event_type::ACTION_APPLIED, Actor::System)
            .with_rule(RuleKind::Action, busy_id.clone());
    block_on(audit.append(busy_fire)).unwrap();

    // "Now" is the present: the quiet rule is 90 days idle (> the 60-day window), the busy rule 0.
    let clock = Arc::new(FakeClock::new(now));
    let engine = DefaultLearningEngine::new(classification, filing, audit.clone(), proposals.clone())
        .with_rules(rules.clone())
        .with_clock(clock);

    let emitted = block_on(engine.propose_candidates(ProposalTrigger::all())).unwrap();
    let stale = emitted
        .iter()
        .find(|p| p.proposal_type == ProposalKind::RetireRule
            && p.target_rule_id.as_ref() == Some(&quiet_id))
        .expect("the long-quiet rule surfaces a stale retire");
    assert_eq!(stale.status, ProposalStatus::PendingReview, "human-gated, never auto");
    assert!(
        stale.rationale.contains("hasn't fired in") && stale.rationale.contains("days"),
        "the card states the idle signal: {}",
        stale.rationale
    );
    // The busy rule fired moments ago → NOT stale, even though it shares the pass.
    assert!(
        !emitted.iter().any(|p| p.proposal_type == ProposalKind::RetireRule
            && p.target_rule_id.as_ref() == Some(&busy_id)),
        "a rule that fired moments ago is not stale"
    );
    // And the stale rule itself is never retired — only proposed.
    use mailmate_ports::storage::rules::RuleRepository;
    let active = block_on(rules.get_active_rules(
        RuleKind::Action,
        mailmate_common::rules::rule::RuleScope::Domain,
    ))
    .unwrap();
    assert!(
        active.iter().any(|r| r.rule_id == quiet_id),
        "stale is proposed, never auto-retired"
    );
}

#[test]
fn a_multi_clause_rule_is_induced_and_shown_with_honest_negative_pool_precision() {
    // Phase 7 EXIT #1, end to end through the real engine + SQLite: the user repeatedly marks
    // mail that BOTH failed auth AND came from no prior contact as `suspicious`. Neither single
    // signal separates it from the negative pool — a known contact's mail also fails auth; a
    // stranger's newsletter also has no prior contact — so induction must compose the conjunction
    // `auth_result == fail AND no_prior_contact == true`, back-tested against that pool.
    let h = harness();
    let t = |b: bool| FeatureValue::Bool(b);
    let s = |x: &str| FeatureValue::Text(x.to_owned());
    for _ in 0..3 {
        block_on(h.engine.record_feedback(TaskFeedback::Classification(classification_row_with(
            "suspicious",
            &[("auth_result", s("fail")), ("no_prior_contact", t(true))],
        ))))
        .unwrap();
    }
    // The negative pool: other-label corrections that each share ONE of the two signals.
    block_on(h.engine.record_feedback(TaskFeedback::Classification(classification_row_with(
        "legit_contact",
        &[("auth_result", s("fail")), ("no_prior_contact", t(false))],
    ))))
    .unwrap();
    block_on(h.engine.record_feedback(TaskFeedback::Classification(classification_row_with(
        "newsletter",
        &[("auth_result", s("pass")), ("no_prior_contact", t(true))],
    ))))
    .unwrap();

    let proposals = block_on(h.engine.propose_candidates(ProposalTrigger {
        source_kind: Some(EvidenceSourceKind::Classification),
        ..ProposalTrigger::all()
    }))
    .unwrap();
    // Only the 3-strong `suspicious` cluster clears the floor; the two pool rows are singletons.
    assert_eq!(proposals.len(), 1, "one induced rule: {proposals:?}");
    let proposal = &proposals[0];
    let draft = proposal.rule_draft.as_ref().unwrap();
    assert_eq!(draft.kind, RuleKind::Classification);
    assert_eq!(draft.effect.set_labels, vec!["suspicious".to_owned()]);

    // The condition is the induced TWO-clause conjunction (not a single domain predicate).
    use mailmate_common::rules::condition::Condition;
    match &draft.condition {
        Condition::All { all } => {
            assert_eq!(all.len(), 2, "a two-clause rule: {:?}", draft.condition);
            let fields: Vec<&str> = all
                .iter()
                .filter_map(|c| match c {
                    Condition::Predicate(p) => Some(p.field.as_str()),
                    _ => None,
                })
                .collect();
            assert!(fields.contains(&"auth_result"), "{fields:?}");
            assert!(fields.contains(&"no_prior_contact"), "{fields:?}");
        }
        other => panic!("expected an induced two-clause All, got {other:?}"),
    }

    // The back-test is stamped honestly: precision 1.0 (the conjunction excludes the pool) and
    // support 3 (the positives reproduced — NOT inflated by any negative-pool false positive).
    let bt = proposal.back_test.expect("an induced rule carries its negative-pool back-test");
    assert_eq!(bt.precision, Some(1.0));
    assert_eq!(bt.support, 3);

    // It persisted with that back-test, so the Review card can render the real numbers.
    let pending = block_on(h.proposals.list_by_status(ProposalStatus::PendingReview)).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].back_test.unwrap().support, 3);
}

#[test]
fn an_induced_rule_that_overlaps_an_active_rule_is_forced_to_human_review_with_the_conflict() {
    // Phase-7 overlap/subsumption: an active rule already labels `auth_result == fail` mail as
    // `spam`. The user then teaches a MORE SPECIFIC rule (auth_fail AND no_prior_contact →
    // suspicious). Every message the induced rule matches also matches the active one — which would
    // label it differently. That co-match-with-a-differing-effect is a conflict the human must
    // adjudicate, so the proposal is forced to PendingHumanReview and carries the conflict for the
    // card; it is NEVER auto-shadowed.
    use mailmate_common::proposal::ProposalKind;
    use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
    use mailmate_common::rules::evaluation::ConflictKind;
    use mailmate_common::rules::rule::{
        HierarchyBand, NewRule, RiskLevel, RuleScope, RuleStatus, RuleVersionContent,
    };
    use mailmate_ports::storage::rules::RuleRepository;

    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let feedback = Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    let classification: Arc<dyn FeedbackRepository<ClassificationFeedback>> = feedback.clone();
    let filing: Arc<dyn FeedbackRepository<FilingFeedback>> = feedback.clone();
    let audit = Arc::new(SqliteAuditRepository::new(backend.clone()));
    let proposals = Arc::new(SqliteProposalRepository::new(backend.clone()));
    let rules = Arc::new(SqliteRuleRepository::new(backend.clone()));
    let engine = DefaultLearningEngine::new(classification, filing, audit.clone(), proposals.clone())
        .with_rules(rules.clone());

    // The existing active GLOBAL classification rule: auth_result == fail → spam.
    let active = block_on(rules.save_rule_draft(NewRule {
        stable_name: "auth-fail-is-spam".to_owned(),
        kind: RuleKind::Classification,
        scope: RuleScope::Global,
        band: HierarchyBand::LearnedActive,
        created_by: Actor::Ai,
        initial_version: RuleVersionContent {
            title: "auth fail → spam".to_owned(),
            description: "label auth failures as spam".to_owned(),
            condition: Condition::Predicate(Predicate {
                field: "auth_result".to_owned(),
                op: Operator::Eq,
                value: FieldValue::Text("fail".to_owned()),
            }),
            effect: RuleEffect {
                set_labels: vec!["spam".to_owned()],
                ..RuleEffect::new()
            },
            priority: 10,
            confidence_threshold: None,
            risk_level: RiskLevel::Low,
            change_reason: "seed".to_owned(),
            created_by: Actor::Ai,
        },
    }))
    .unwrap();
    block_on(rules.update_rule_status(&active, RuleKind::Classification, RuleStatus::Active)).unwrap();

    // The user induces a more-specific rule with a DIFFERENT label.
    let t = |b: bool| FeatureValue::Bool(b);
    let s = |x: &str| FeatureValue::Text(x.to_owned());
    for _ in 0..3 {
        block_on(engine.record_feedback(TaskFeedback::Classification(classification_row_with(
            "suspicious",
            &[("auth_result", s("fail")), ("no_prior_contact", t(true))],
        ))))
        .unwrap();
    }
    block_on(engine.record_feedback(TaskFeedback::Classification(classification_row_with(
        "legit_contact",
        &[("auth_result", s("fail")), ("no_prior_contact", t(false))],
    ))))
    .unwrap();
    block_on(engine.record_feedback(TaskFeedback::Classification(classification_row_with(
        "newsletter",
        &[("auth_result", s("pass")), ("no_prior_contact", t(true))],
    ))))
    .unwrap();

    let proposals_out = block_on(engine.propose_candidates(ProposalTrigger {
        source_kind: Some(EvidenceSourceKind::Classification),
        ..ProposalTrigger::all()
    }))
    .unwrap();
    let induced = proposals_out
        .iter()
        .find(|p| p.proposal_type == ProposalKind::NewRule)
        .expect("the suspicious cluster induces a rule");
    assert!(
        !induced.conflicts.is_empty(),
        "the induced rule conflicts with the active auth_fail→spam rule"
    );
    assert_eq!(induced.conflicts[0].kind, ConflictKind::Overlap);
    assert_eq!(
        induced.conflicts[0].existing_rule_id.as_ref(),
        Some(&active),
        "the conflict names the active rule it overlaps"
    );
    assert_eq!(
        induced.recommended_status,
        RuleStatus::PendingHumanReview,
        "a conflicting proposal is forced to human review, never auto-shadowed"
    );
    assert!(induced.risk_level >= RiskLevel::Medium, "the overlap raises the risk");

    // It persisted WITH its conflicts (round-tripped through the proposal JSON), so the card renders
    // the conflict measure after a reload.
    let pending = block_on(proposals.list_by_status(ProposalStatus::PendingReview)).unwrap();
    let reloaded = pending
        .iter()
        .find(|p| p.proposal_type == ProposalKind::NewRule)
        .unwrap();
    assert!(!reloaded.conflicts.is_empty(), "conflicts survive a reload");
}

#[test]
fn account_stamped_corrections_induce_an_account_scoped_rule() {
    // Phase-7 7a per-account scope, end to end: corrections captured with an `account_id` salient
    // feature cluster per account, so the induced rule is scoped to that account — not a false
    // Global rule that would also fire on the user's other accounts.
    use mailmate_common::rules::rule::RuleScope;
    let h = harness();
    let s = |x: &str| FeatureValue::Text(x.to_owned());
    // Three "suspicious" corrections, all on the "work" account, all sharing auth_result == fail.
    for _ in 0..3 {
        block_on(h.engine.record_feedback(TaskFeedback::Classification(classification_row_with(
            "suspicious",
            &[("account_id", s("work")), ("auth_result", s("fail"))],
        ))))
        .unwrap();
    }
    // A negative-pool row on the SAME account that auth_result==fail alone must not mis-fire on.
    block_on(h.engine.record_feedback(TaskFeedback::Classification(classification_row_with(
        "legit",
        &[("account_id", s("work")), ("auth_result", s("pass"))],
    ))))
    .unwrap();

    let proposals = block_on(h.engine.propose_candidates(ProposalTrigger {
        source_kind: Some(EvidenceSourceKind::Classification),
        ..ProposalTrigger::all()
    }))
    .unwrap();
    let induced = proposals
        .iter()
        .find(|p| p.proposal_type == mailmate_common::proposal::ProposalKind::NewRule)
        .expect("the work-account cluster induces a rule");
    let draft = induced.rule_draft.as_ref().unwrap();
    assert_eq!(draft.scope, RuleScope::Account, "scoped to the account, not Global");
    // The induced condition keys on the real feature (auth_result), NEVER account_id (that binds
    // via scope; the runtime field environment carries no account_id).
    use mailmate_common::rules::condition::Condition;
    let fields: Vec<String> = match &draft.condition {
        Condition::Predicate(p) => vec![p.field.clone()],
        Condition::All { all } => all
            .iter()
            .filter_map(|c| match c {
                Condition::Predicate(p) => Some(p.field.clone()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    };
    assert!(fields.iter().any(|f| f == "auth_result"), "{fields:?}");
    assert!(!fields.iter().any(|f| f == "account_id"), "account_id is scope, not a predicate: {fields:?}");
}

#[test]
fn an_ambiguous_cluster_that_cannot_beat_its_negative_pool_is_withheld() {
    // The negative control: the user marks `suspicious` mail that all failed auth — but so did a
    // batch of mail they labelled `legit_contact`. No induced clause separates the two, so the
    // best candidate's precision sits below the bar and NOTHING is proposed (degrade, don't lie).
    let h = harness();
    let s = |x: &str| FeatureValue::Text(x.to_owned());
    for _ in 0..3 {
        block_on(h.engine.record_feedback(TaskFeedback::Classification(classification_row_with(
            "suspicious",
            &[("auth_result", s("fail"))],
        ))))
        .unwrap();
    }
    for _ in 0..3 {
        block_on(h.engine.record_feedback(TaskFeedback::Classification(classification_row_with(
            "legit_contact",
            &[("auth_result", s("fail"))],
        ))))
        .unwrap();
    }
    let proposals = block_on(h.engine.propose_candidates(ProposalTrigger {
        source_kind: Some(EvidenceSourceKind::Classification),
        ..ProposalTrigger::all()
    }))
    .unwrap();
    assert!(
        proposals.is_empty(),
        "an inseparable cluster fails the negative-pool precision bar: {proposals:?}"
    );
    let pending = block_on(h.proposals.list_by_status(ProposalStatus::PendingReview)).unwrap();
    assert!(pending.is_empty(), "nothing persisted for the withheld candidate");
}

#[test]
fn propose_candidates_is_idempotent_across_passes() {
    // The host runs a proposal pass at launch and on every tick. A recurring cluster must
    // propose exactly once — re-running over the same feedback must not duplicate.
    let h = harness();
    for _ in 0..3 {
        block_on(
            h.engine
                .record_feedback(TaskFeedback::Filing(filing_row("stripe.com", "Receipts"))),
        )
        .unwrap();
    }

    let first = block_on(h.engine.propose_candidates(ProposalTrigger::all())).unwrap();
    assert_eq!(
        first.len(),
        1,
        "first pass proposes the crossed-threshold cluster"
    );

    let second = block_on(h.engine.propose_candidates(ProposalTrigger::all())).unwrap();
    assert!(
        second.is_empty(),
        "a recurring cluster proposes once, not once per pass: {second:?}"
    );

    // The store holds exactly one pending proposal — no duplicate accumulated.
    let pending = block_on(h.proposals.list_by_status(ProposalStatus::PendingReview)).unwrap();
    assert_eq!(
        pending.len(),
        1,
        "no duplicate proposal piled up across passes"
    );
}

#[test]
fn a_reviewed_cluster_is_not_re_proposed() {
    // Feedback rows persist after a proposal is reviewed. A rejected cluster must not be nagged
    // again on the next pass (the invariant on `ProposalStatus::Rejected`).
    let h = harness();
    for _ in 0..3 {
        block_on(
            h.engine
                .record_feedback(TaskFeedback::Filing(filing_row("stripe.com", "Receipts"))),
        )
        .unwrap();
    }

    let first = block_on(h.engine.propose_candidates(ProposalTrigger::all())).unwrap();
    assert_eq!(first.len(), 1);
    // A human rejects it.
    block_on(h.proposals.set_status(
        &first[0].id,
        ProposalStatus::Rejected,
        Some(Timestamp::now()),
    ))
    .unwrap();

    let again = block_on(h.engine.propose_candidates(ProposalTrigger::all())).unwrap();
    assert!(
        again.is_empty(),
        "a rejected cluster is not re-proposed without new evidence: {again:?}"
    );
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
fn the_promotion_gate_blocks_a_proposal_whose_domain_history_contradicts_it() {
    // The negative control the standalone back-test test cannot prove: that propose_candidates
    // ACTUALLY back-tests before surfacing. The user files `noisy.test` inconsistently — 3 moves
    // to "Archive" (which alone crosses min_filing_moves) PLUS 2 moves to "Keep". A
    // `noisy.test → Archive` rule would mis-file the 2 "Keep" messages, so its precision over the
    // domain's whole history is 3/5 = 0.6, below the 0.9 bar. It must NOT be proposed.
    let h = harness();
    for _ in 0..3 {
        block_on(
            h.engine
                .record_feedback(TaskFeedback::Filing(filing_row("noisy.test", "Archive"))),
        )
        .unwrap();
    }
    for _ in 0..2 {
        block_on(
            h.engine
                .record_feedback(TaskFeedback::Filing(filing_row("noisy.test", "Keep"))),
        )
        .unwrap();
    }

    let proposals = block_on(h.engine.propose_candidates(ProposalTrigger::all())).unwrap();
    assert!(
        proposals.is_empty(),
        "an inconsistently-filed domain fails the precision bar and is not proposed: {proposals:?}"
    );
    // The gate runs BEFORE persist, so nothing was written either.
    let pending = block_on(h.proposals.list_by_status(ProposalStatus::PendingReview)).unwrap();
    assert!(
        pending.is_empty(),
        "no proposal persisted for the back-test-failing candidate"
    );
}

#[test]
fn a_consistent_domain_history_clears_the_promotion_gate_and_proposes() {
    // The positive control: a domain filed consistently to one folder clears the now-wired
    // back-test gate (proving the gate blocks the bad and passes the good, not just blocks all).
    let h = harness();
    for _ in 0..3 {
        block_on(
            h.engine
                .record_feedback(TaskFeedback::Filing(filing_row("stripe.com", "Receipts"))),
        )
        .unwrap();
    }
    let proposals = block_on(h.engine.propose_candidates(ProposalTrigger::all())).unwrap();
    assert_eq!(proposals.len(), 1, "a clean history clears the back-test gate");
    assert_eq!(
        proposals[0].rule_draft.as_ref().unwrap().effect.move_to.as_deref(),
        Some("Receipts")
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

