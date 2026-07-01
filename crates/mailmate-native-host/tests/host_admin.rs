//! Behavioural tests for the Phase-12 management surface of the [`HostRouter`]:
//! `explain_decision` (the audit timeline for a message), `list_pending_reviews` (the review
//! queue), and `get_settings` (the secret-free settings snapshot) — driven over a *real*
//! `build_router` composition against an in-memory database, with the stores seeded directly.

use std::sync::Arc;

use futures::executor::block_on;
use serde_json::{json, Value};

use mailmate_common::actor::Actor;
use mailmate_common::audit::AuditEntry;
use mailmate_common::features::FeatureValue;
use mailmate_common::feedback::{
    ClassificationFeedback, ClassificationFeedbackQuery, FeedbackPolarity, FilingFeedback,
    FilingFeedbackQuery, FilingFeedbackRow, PinnedVersions,
};
use mailmate_common::ids::{FolderId, MessageId, ProposalId};
use mailmate_common::message::ClassificationStatus;
use mailmate_common::proposal::{AgentProposal, ProposalKind, ProposalStatus};
use mailmate_common::protocol::{Frame, ProtocolVersion, ResponseStatus};
use mailmate_common::rules::rule::{RiskLevel, RuleStatus};
use mailmate_common::time::Timestamp;
use mailmate_native_host::config::{AppConfig, CategoryPolicy};
use mailmate_native_host::router::HostRouter;
use mailmate_native_host::runtime::build_router;
use mailmate_ports::storage::messages::MessageRepository;
use mailmate_ports::storage::rules::RuleRepository;
use mailmate_ports::storage::{AuditRepository, FeedbackRepository, ProposalRepository};
use mailmate_storage::{
    open_and_migrate, SqliteAuditRepository, SqliteBackend, SqliteFeedbackRepository,
    SqliteMessageRepository, SqliteProposalRepository, SqliteRuleRepository, StorageConfig,
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
    match &out.sent_frames()[0] {
        Frame::Response {
            status: ResponseStatus::Error,
            error: Some(e),
            ..
        } => e.code.clone(),
        other => panic!("expected an error response, got {other:?}"),
    }
}

/// A fresh in-memory backend + a router built over it (with admin + follow-ups wired). Each call
/// gets a UNIQUE, clean secrets file so `set_secret` writes never leak across tests or runs.
fn router_over(backend: &Arc<SqliteBackend>, out: Arc<FakeTransport>) -> HostRouter {
    static SECRET_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = SECRET_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let secrets = std::env::temp_dir().join(format!(
        "mailmate-admin-secrets-{}-{n}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&secrets); // start clean
                                            // config_path / tier2 weights: None — these tests exercise the in-memory config + an
                                            // in-memory Tier-2 model (no on-disk persistence).
    build_router(&AppConfig::default(), backend, out, secrets, None, None).unwrap()
}

fn pending_proposal(id: &str, title: &str) -> AgentProposal {
    AgentProposal {
        id: ProposalId::from(id),
        proposal_type: ProposalKind::NewRule,
        status: ProposalStatus::PendingReview,
        title: title.to_owned(),
        rationale: "repeated user filings justify this rule".to_owned(),
        risk_level: RiskLevel::Low,
        recommended_status: RuleStatus::ShadowMode,
        rule_draft: None,
        target_rule_kind: None,
        target_rule_id: None,
        workflow_draft: None,
        target_workflow_id: None,
        evidence_refs: vec![],
        back_test: Some(mailmate_common::proposal::BackTest {
            precision: Some(0.92),
            support: 12,
        }),
        conflicts: Vec::new(),
        source_provider: "test".to_owned(),
        created_at: Timestamp::now(),
        reviewed_at: None,
    }
}

#[test]
fn explain_decision_returns_the_audit_timeline_for_a_message() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    // Seed an audited action for the message the explanation will target.
    let audit = SqliteAuditRepository::new(backend.clone());
    block_on(
        audit.append(
            AuditEntry::new("action_applied", Actor::System)
                .with_message(MessageId::from("msg_tb_42"))
                .with_payload(json!({ "kind": "tag" })),
        ),
    )
    .unwrap();

    block_on(router.handle(request(
        "explain_decision",
        json!({ "thunderbird_message_id": "42" }),
    )))
    .unwrap();

    let payload = one_ok_response(&out);
    assert_eq!(payload["message_id"], "msg_tb_42");
    let timeline = payload["timeline"].as_array().unwrap();
    assert_eq!(timeline.len(), 1);
    assert_eq!(timeline[0]["event_type"], "action_applied");
    assert_eq!(timeline[0]["payload"]["kind"], "tag");
}

#[test]
fn list_pending_reviews_returns_the_proposals_awaiting_a_human() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    let proposals = SqliteProposalRepository::new(backend.clone());
    block_on(proposals.save(pending_proposal("prop_1", "Tag receipts"), vec![])).unwrap();

    block_on(router.handle(request("list_pending_reviews", json!({})))).unwrap();

    let payload = one_ok_response(&out);
    let pending = payload["pending_reviews"].as_array().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["id"], "prop_1");
    assert_eq!(pending[0]["title"], "Tag receipts");
    assert_eq!(pending[0]["proposal_type"], "new_rule");
    assert_eq!(pending[0]["risk_level"], "low");
    // Phase 6: the card carries the back-test the gate admitted the candidate on, so it can be
    // approved without drilling into the detail view.
    assert_eq!(pending[0]["back_test"]["precision"], 0.92);
    assert_eq!(pending[0]["back_test"]["support"], 12);
}

#[test]
fn a_rule_you_keep_undoing_surfaces_a_retire_proposal_on_the_review_queue() {
    // Phase 7 EXIT #2, end to end through the REAL composition root: an active rule whose
    // auto-applied action the user keeps undoing decays. A proposal pass (the per-tick/catch-up
    // entry point) surfaces a human-gated `retire_rule` proposal naming that rule on the review
    // queue — and the rule itself stays ACTIVE (proposed, never auto-retired).
    use mailmate_common::audit::event_type;
    use mailmate_common::rules::rule::{RuleKind, RuleScope};

    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    let rule_id = seed_active_rule(&backend, "file_noisy", "noisy.example", "Spam");

    // Three undos of this rule's action — each stamps an `action_undone` row against its rule_id.
    let audit = SqliteAuditRepository::new(backend.clone());
    for _ in 0..3 {
        block_on(
            audit.append(
                AuditEntry::new(event_type::ACTION_UNDONE, Actor::User)
                    .with_rule(RuleKind::Action, rule_id.clone()),
            ),
        )
        .unwrap();
    }

    // The pass announces the retire as a `proposal_ready` notification (asserted below); take the
    // frame count now so we can read the list response that follows it.
    block_on(router.generate_proposals()).unwrap();
    let after_pass = out.sent_frames().len();

    // The review queue carries the retire proposal, naming the rule it targets.
    block_on(router.handle(request("list_pending_reviews", json!({})))).unwrap();
    let frames = out.sent_frames();
    let payload = match frames.last().expect("a list response frame") {
        Frame::Response {
            status: ResponseStatus::Ok,
            payload: Some(payload),
            ..
        } => payload.clone(),
        other => panic!("expected an ok list response, got {other:?}"),
    };
    assert!(
        after_pass >= 1,
        "the pass also announced the retire as a proposal_ready notification"
    );
    let pending = payload["pending_reviews"].as_array().unwrap();
    let retire = pending
        .iter()
        .find(|p| p["proposal_type"] == "retire_rule")
        .expect("a retire proposal surfaced on the review queue");
    assert_eq!(retire["target_rule_id"], rule_id.as_str());
    assert_eq!(retire["recommended_status"], "retired");

    // The rule is still ACTIVE — the engine proposed retirement, it did not perform it.
    let rules = SqliteRuleRepository::new(backend.clone());
    let active = block_on(rules.get_active_rules(RuleKind::Action, RuleScope::Domain)).unwrap();
    assert!(
        active.iter().any(|r| r.rule_id == rule_id),
        "human approval is the only path to retirement"
    );
}

#[test]
fn a_rule_that_has_gone_quiet_surfaces_a_stale_retire_through_the_real_root() {
    // Phase-7 7b DetectStale through the REAL composition root: the runtime wires a clock into the
    // learning engine (`.with_clock`), so a proposal pass also surfaces a human-gated retire for a
    // live rule that has not fired in the idle window. The audit store preserves a row's
    // `created_at`, so we backdate this rule's single fire ~90 days; the real `SystemClock`'s "now"
    // is then well past the 60-day window. (If the runtime forgot to wire the clock, no stale
    // proposal would surface and this fails — it guards that wiring.)
    use mailmate_common::audit::event_type;
    use mailmate_common::rules::rule::RuleKind;

    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    let rule_id = seed_active_rule(&backend, "file_quiet", "quiet.example", "Archive");

    // One fire, backdated 90 days — the rule used to work, then went silent.
    let audit = SqliteAuditRepository::new(backend.clone());
    let mut fire = AuditEntry::new(event_type::ACTION_APPLIED, Actor::System)
        .with_rule(RuleKind::Action, rule_id.clone());
    fire.created_at = Timestamp::now().add_days(-90);
    block_on(audit.append(fire)).unwrap();

    block_on(router.generate_proposals()).unwrap();

    // The review queue carries the stale retire, naming the rule it targets (the proposal_ready
    // notification omits target_rule_id, so we read the list — as the undo-decay probe does).
    block_on(router.handle(request("list_pending_reviews", json!({})))).unwrap();
    let frames = out.sent_frames();
    let payload = match frames.last().expect("a list response frame") {
        Frame::Response {
            status: ResponseStatus::Ok,
            payload: Some(payload),
            ..
        } => payload.clone(),
        other => panic!("expected an ok list response, got {other:?}"),
    };
    let stale = payload["pending_reviews"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["proposal_type"] == "retire_rule" && p["target_rule_id"] == rule_id.as_str())
        .expect("a long-quiet rule surfaces a stale retire through the wired clock");
    let rationale = stale["rationale"].as_str().unwrap_or_default();
    assert!(
        rationale.contains("hasn't fired in"),
        "the stale card states the idle signal: {rationale}"
    );
    assert_eq!(stale["recommended_status"], "retired");
}

#[test]
fn repeated_record_sent_mail_surfaces_a_vip_proposal_through_the_real_root() {
    // Phase-7 learn-from-Sent through the REAL composition root: the extension reports each send
    // via `record_sent_mail`; the host records outbound evidence, and a proposal pass surfaces a
    // human-gated VIP/priority rule for a domain the user emails repeatedly.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    // Three separate sends to acme.com (the default bar). The ACK reports the rows recorded.
    for _ in 0..3 {
        block_on(router.handle(request(
            "record_sent_mail",
            json!({ "recipients": ["Boss <boss@Acme.com>"], "subject": "re: budget" }),
        )))
        .unwrap();
        assert_eq!(last_ok(&out)["recorded"], 1, "one mail_sent row per send");
    }

    block_on(router.generate_proposals()).unwrap();

    block_on(router.handle(request("list_pending_reviews", json!({})))).unwrap();
    let frames = out.sent_frames();
    let payload = match frames.last().expect("a list response frame") {
        Frame::Response {
            status: ResponseStatus::Ok,
            payload: Some(payload),
            ..
        } => payload.clone(),
        other => panic!("expected an ok list response, got {other:?}"),
    };
    let vip = payload["pending_reviews"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| {
            p["proposal_type"] == "new_rule"
                && p["title"].as_str().is_some_and(|t| t.contains("acme.com"))
        })
        .expect("a frequently-emailed domain surfaces a VIP proposal");
    // The candidate is a classification rule keyed on the (case-folded) domain, setting priority.
    assert_eq!(vip["rule_draft"]["condition"]["field"], "sender_domain");
    assert_eq!(vip["rule_draft"]["condition"]["value"], "acme.com");
    assert_eq!(vip["rule_draft"]["effect"]["priority"], "high");
    assert_eq!(
        vip["recommended_status"], "shadow_mode",
        "never auto-activated"
    );
}

#[test]
fn a_proposals_card_carries_its_rule_drafts_condition_and_effect_ast() {
    // A proposal with a filing rule_draft surfaces its condition→effect AST on the card so the
    // extension can render the rule in English (the Phase-6 exit: a proposal card shows its rule).
    use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
    use mailmate_common::rules::effect::RuleEffect;
    use mailmate_common::rules::rule::{RuleDraft, RuleKind, RuleScope};

    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    let mut proposal = pending_proposal("prop_ast", "File stripe.com mail to Receipts");
    proposal.rule_draft = Some(RuleDraft {
        kind: RuleKind::Action,
        scope: RuleScope::Domain,
        condition: Condition::Predicate(Predicate {
            field: "sender_domain".to_owned(),
            op: Operator::Eq,
            value: FieldValue::Text("stripe.com".to_owned()),
        }),
        effect: RuleEffect {
            move_to: Some("Receipts".to_owned()),
            ..RuleEffect::new()
        },
    });
    let proposals = SqliteProposalRepository::new(backend.clone());
    block_on(proposals.save(proposal, vec![])).unwrap();

    block_on(router.handle(request("list_pending_reviews", json!({})))).unwrap();
    let payload = one_ok_response(&out);
    let card = &payload["pending_reviews"][0];
    // The condition AST rides on the card (a bare predicate: sender_domain == stripe.com)…
    assert_eq!(card["rule_draft"]["condition"]["field"], "sender_domain");
    assert_eq!(card["rule_draft"]["condition"]["op"], "eq");
    assert_eq!(card["rule_draft"]["condition"]["value"], "stripe.com");
    // …and the effect AST (move → Receipts), so the extension renders it without a second call.
    assert_eq!(card["rule_draft"]["effect"]["move"], "Receipts");
}

/// Seed an active action rule (sender_domain == `domain` → move to `folder`) directly into the
/// store, returning its id. Mirrors what proposal-acceptance produces, without the whole flow.
fn seed_active_rule(
    backend: &Arc<SqliteBackend>,
    stable_name: &str,
    domain: &str,
    folder: &str,
) -> mailmate_common::ids::RuleId {
    use mailmate_common::actor::Actor;
    use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
    use mailmate_common::rules::effect::RuleEffect;
    use mailmate_common::rules::rule::{
        HierarchyBand, NewRule, RiskLevel, RuleKind, RuleScope, RuleStatus, RuleVersionContent,
    };
    use mailmate_ports::storage::rules::RuleRepository;

    let repo = SqliteRuleRepository::new(backend.clone());
    let rule_id = block_on(repo.save_rule_draft(NewRule {
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
                move_to: Some(folder.to_owned()),
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
    block_on(repo.update_rule_status(&rule_id, RuleKind::Action, RuleStatus::Active)).unwrap();
    rule_id
}

#[test]
fn list_rules_returns_active_rules_with_their_condition_and_a_correction_count() {
    // The Phase-6 exit (half 1): an approved (active) rule appears in the Rules manager with its
    // condition→effect AST (the tab renders it in English) and its backed correction signal.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());
    let _rule_id = seed_active_rule(&backend, "file_stripe", "stripe.com", "Receipts");

    block_on(router.handle(request("list_rules", json!({})))).unwrap();
    let payload = one_ok_response(&out);
    let rules = payload["rules"].as_array().unwrap();
    let card = rules
        .iter()
        .find(|r| r["effect"]["move"] == "Receipts")
        .expect("the seeded rule is listed");
    assert_eq!(card["status"], "active");
    assert_eq!(card["kind"], "action");
    assert_eq!(card["condition"]["field"], "sender_domain");
    assert_eq!(card["condition"]["value"], "stripe.com");
    // No undos yet → a real zero (the audit column is now stamped, so this is a confident count,
    // not "unknown").
    assert_eq!(card["undo_count"], 0);
}

#[test]
fn set_rule_status_disables_a_rule_and_it_stays_visible_for_re_enabling() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());
    let rule_id = seed_active_rule(&backend, "file_stripe", "stripe.com", "Receipts");

    // Disable it.
    block_on(router.handle(request(
        "set_rule_status",
        json!({ "rule_id": rule_id.as_str(), "kind": "action", "status": "disabled" }),
    )))
    .unwrap();
    let resp = last_ok(&out);
    assert_eq!(resp["status"], "disabled");
    assert_eq!(
        resp["reloaded"], true,
        "the engines hot-reloaded so it stops firing now"
    );

    // It is STILL visible in the manager (so a human can re-enable it) — disabled, not vanished.
    block_on(router.handle(request("list_rules", json!({})))).unwrap();
    let rules = last_ok(&out)["rules"].as_array().unwrap().clone();
    let card = rules
        .iter()
        .find(|r| r["rule_id"] == rule_id.as_str())
        .expect("still listed");
    assert_eq!(card["status"], "disabled");

    // Re-enable it.
    block_on(router.handle(request(
        "set_rule_status",
        json!({ "rule_id": rule_id.as_str(), "kind": "action", "status": "active" }),
    )))
    .unwrap();
    assert_eq!(last_ok(&out)["status"], "active");
}

#[test]
fn set_rule_status_rejects_a_lifecycle_internal_status() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());
    let rule_id = seed_active_rule(&backend, "file_stripe", "stripe.com", "Receipts");

    // `retired`/`draft`/`pending_human_review` are not user-settable from the manager.
    block_on(router.handle(request(
        "set_rule_status",
        json!({ "rule_id": rule_id.as_str(), "kind": "action", "status": "retired" }),
    )))
    .unwrap();
    assert_eq!(last_error_code(&out), "invalid_payload");
}

#[test]
fn set_rule_status_refuses_to_activate_an_unreviewed_draft_rule() {
    // The review-gate guard: a rule still in `draft` (never approved) cannot be activated via the
    // manager — that would bypass the proposal-review materialization gate. Only rules already
    // active/shadow/disabled are manageable here.
    use mailmate_common::actor::Actor;
    use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
    use mailmate_common::rules::effect::RuleEffect;
    use mailmate_common::rules::rule::{
        HierarchyBand, NewRule, RiskLevel, RuleKind, RuleScope, RuleVersionContent,
    };
    use mailmate_ports::storage::rules::RuleRepository;

    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    // A bare draft rule (never reviewed/activated).
    let repo = SqliteRuleRepository::new(backend.clone());
    let rule_id = block_on(repo.save_rule_draft(NewRule {
        stable_name: "sneaky_draft".to_owned(),
        kind: RuleKind::Action,
        scope: RuleScope::Domain,
        band: HierarchyBand::LearnedActive,
        created_by: Actor::Ai,
        initial_version: RuleVersionContent {
            title: "x".to_owned(),
            description: "x".to_owned(),
            condition: Condition::Predicate(Predicate {
                field: "sender_domain".to_owned(),
                op: Operator::Eq,
                value: FieldValue::Text("evil.example".to_owned()),
            }),
            effect: RuleEffect {
                move_to: Some("Inbox".to_owned()),
                ..RuleEffect::new()
            },
            priority: 1,
            confidence_threshold: None,
            risk_level: RiskLevel::Low,
            change_reason: "x".to_owned(),
            created_by: Actor::Ai,
        },
    }))
    .unwrap();

    block_on(router.handle(request(
        "set_rule_status",
        json!({ "rule_id": rule_id.as_str(), "kind": "action", "status": "active" }),
    )))
    .unwrap();
    assert_eq!(last_error_code(&out), "rule_not_manageable");

    // And it did NOT become active.
    assert!(
        block_on(repo.get_active_rules(RuleKind::Action, RuleScope::Domain))
            .unwrap()
            .is_empty(),
        "the draft rule was not activated"
    );
}

#[test]
fn an_undo_of_a_rules_action_is_counted_against_that_rule_in_the_manager() {
    // The backed correction signal end-to-end: record an Undo naming the rule, then list_rules
    // shows undo_count == 1 (the audit row's rule_id column is now stamped + queried).
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());
    let rule_a = seed_active_rule(&backend, "file_stripe", "stripe.com", "Receipts");
    // A SECOND rule, to prove the count is attributed per-rule (not a global undo tally).
    let rule_b = seed_active_rule(&backend, "file_github", "github.com", "Code");

    block_on(router.handle(request(
        "record_user_action",
        json!({
            "event_type": "action_undone",
            "action_kind": "move",
            "thunderbird_message_id": "tb_u",
            "from_folder_id": "Receipts",
            "to_folder_id": "inbox",
            "rule_id": rule_a.as_str(),
            "user_initiated": true,
        }),
    )))
    .unwrap();
    // The undo took the real route_undo path (filing correction), not the audit-only fallback —
    // so the test exercises the genuine data flow, not just an audit side-write.
    assert_eq!(
        last_ok(&out)["sink"],
        "filing_feedback",
        "route_undo handled the move undo"
    );

    // The undo's learning signal really landed as a negative filing-feedback row.
    let filing = SqliteFeedbackRepository::new(backend.clone());
    let rows = block_on(FeedbackRepository::<FilingFeedback>::query(
        &filing,
        FilingFeedbackQuery::default(),
    ))
    .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the undo wrote exactly one filing-feedback row"
    );

    block_on(router.handle(request("list_rules", json!({})))).unwrap();
    let rules = last_ok(&out)["rules"].as_array().unwrap().clone();
    let card_a = rules
        .iter()
        .find(|r| r["rule_id"] == rule_a.as_str())
        .unwrap();
    let card_b = rules
        .iter()
        .find(|r| r["rule_id"] == rule_b.as_str())
        .unwrap();
    assert_eq!(card_a["undo_count"], 1, "the undo is attributed to rule A");
    assert_eq!(
        card_b["undo_count"], 0,
        "an unrelated rule is NOT charged the undo"
    );
}

#[test]
fn get_settings_returns_the_secret_free_snapshot() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    block_on(router.handle(request("get_settings", json!({})))).unwrap();

    let payload = one_ok_response(&out);
    assert_eq!(payload["retention_level"], "metadata");
    assert_eq!(payload["catch_up_on_launch"], true);
    assert_eq!(payload["follow_up_tick_seconds"], 0);
    assert!(payload["providers"].as_array().unwrap().is_empty());
    // The category vocabulary rides along so the panel renders human names without hardcoding.
    let categories = payload["categories"]
        .as_array()
        .expect("categories vocabulary");
    assert!(!categories.is_empty());
    assert!(categories
        .iter()
        .any(|c| c["key"] == "newsletters" && c["label"] == "Newsletters"));
}

#[test]
fn hello_advertises_wired_capabilities_and_a_configured_provider() {
    use mailmate_native_host::config::{AiSettings, ProviderSettings};
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    // A config with a configured default provider → drafting is enabled in config.
    let config = AppConfig {
        ai: AiSettings {
            default_provider: Some("local".to_owned()),
            providers: vec![ProviderSettings {
                id: "local".to_owned(),
                kind: "mock".to_owned(),
                endpoint: None,
                model: None,
            }],
        },
        ..AppConfig::default()
    };
    let secrets = std::env::temp_dir().join("mailmate-hello-test-secrets.json");
    let router = build_router(&config, &backend, out.clone(), secrets, None, None).unwrap();

    block_on(router.handle(request("hello", json!({})))).unwrap();

    let payload = one_ok_response(&out);
    assert_eq!(payload["protocol_version"], "1.0");
    assert_eq!(payload["drafting_available"], true);
    assert_eq!(payload["retention_level"], "metadata");
    let caps: Vec<&str> = payload["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    // The full composition root wires both suites, so hello advertises them.
    assert!(caps.contains(&"followups"));
    assert!(caps.contains(&"list_pending_reviews"));
    assert!(caps.contains(&"get_settings"));
    // The Phase-5 triage-tuning verbs are advertised when the admin surface is wired.
    assert!(caps.contains(&"set_category_policy"));
    assert!(caps.contains(&"set_account_scope"));
    assert!(caps.contains(&"set_tag_mapping"));
}

#[test]
fn explain_decision_without_a_message_id_is_an_invalid_payload() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());
    block_on(router.handle(request("explain_decision", json!({})))).unwrap();
    assert_eq!(one_error_code(&out), "invalid_payload");
}

#[test]
fn admin_requests_without_the_suite_are_rejected() {
    // A bare router (no admin wired) answers the admin types with admin_not_configured.
    let out = Arc::new(FakeTransport::new());
    let bare = bare_router(out.clone());
    block_on(bare.handle(request("get_settings", json!({})))).unwrap();
    assert_eq!(one_error_code(&out), "admin_not_configured");
}

/// A router with NO admin/follow-up wiring (the Phase-10 shape over fakes).
fn bare_router(out: Arc<FakeTransport>) -> HostRouter {
    use mailmate_core::Ports;
    use mailmate_test_support::fakes::{
        FakeActionPlanner, FakeAuditRepository, FakeClassificationEngine, FakeClock,
        FakeLearningEngine, FakeMailClient, FakePolicyGuard, FakeProposalReview, FakeReplyDrafter,
        FakeRuleCurator, FakeSecretStore, FakeTier2Classifier, FakeTrainingPipeline,
        StubFeatureExtractor,
    };

    let ports = Ports {
        mail_client: Arc::new(FakeMailClient::new()),
        transport: out.clone(),
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
    };
    HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out)
}

fn neutral() -> mailmate_common::classification::Classification {
    use mailmate_common::classification::{Classification, ClassificationProvenance, Priority};
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

// The settings-write tests share one router (so the in-memory config persists across calls) and
// read the LATEST response frame each time.
fn last_ok(out: &FakeTransport) -> Value {
    match out.sent_frames().last().expect("a frame") {
        Frame::Response {
            status: ResponseStatus::Ok,
            payload: Some(payload),
            ..
        } => payload.clone(),
        other => panic!("expected an ok response, got {other:?}"),
    }
}

fn last_error_code(out: &FakeTransport) -> String {
    match out.sent_frames().last().expect("a frame") {
        Frame::Response {
            status: ResponseStatus::Error,
            error: Some(e),
            ..
        } => e.code.clone(),
        other => panic!("expected an error response, got {other:?}"),
    }
}

#[test]
fn set_pause_engages_the_kill_switch_and_get_settings_reflects_it() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    block_on(router.handle(request("set_pause", json!({ "paused": true })))).unwrap();
    assert_eq!(last_ok(&out)["paused"], true);

    block_on(router.handle(request("get_settings", json!({})))).unwrap();
    assert_eq!(last_ok(&out)["paused"], true);
}

#[test]
fn set_settings_writes_retention_and_rejects_an_unknown_level() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    // A body-retaining level needs consent to take effect (the consent gate), so grant it here.
    block_on(router.handle(request(
        "set_settings",
        json!({ "retention_level": "summaries", "body_consent": true, "follow_up_tick_seconds": 600 }),
    )))
    .unwrap();
    let payload = last_ok(&out);
    assert_eq!(payload["settings"]["retention_level"], "summaries");
    assert_eq!(payload["settings"]["follow_up_tick_seconds"], 600);
    assert_eq!(payload["settings"]["updated"], true);

    block_on(router.handle(request(
        "set_settings",
        json!({ "retention_level": "telepathic" }),
    )))
    .unwrap();
    assert_eq!(last_error_code(&out), "invalid_payload");
}

#[test]
fn set_category_policy_persists_and_get_settings_reflects_it() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    // Disable a starter category.
    block_on(router.handle(request(
        "set_category_policy",
        json!({ "category": "newsletters", "policy": "off" }),
    )))
    .unwrap();
    assert_eq!(
        last_ok(&out)["settings"]["category_policies"]["newsletters"],
        "off"
    );

    // get_settings reflects it.
    block_on(router.handle(request("get_settings", json!({})))).unwrap();
    assert_eq!(last_ok(&out)["category_policies"]["newsletters"], "off");

    // Setting it back to `auto` REMOVES the entry (the map stays sparse).
    block_on(router.handle(request(
        "set_category_policy",
        json!({ "category": "newsletters", "policy": "auto" }),
    )))
    .unwrap();
    block_on(router.handle(request("get_settings", json!({})))).unwrap();
    assert!(
        last_ok(&out)["category_policies"]
            .get("newsletters")
            .is_none(),
        "auto is the default, stored as absence"
    );
}

#[test]
fn set_category_policy_rejects_an_unknown_category_or_policy() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    block_on(router.handle(request(
        "set_category_policy",
        json!({ "category": "not-a-real-category", "policy": "off" }),
    )))
    .unwrap();
    assert_eq!(last_error_code(&out), "invalid_payload");

    block_on(router.handle(request(
        "set_category_policy",
        json!({ "category": "newsletters", "policy": "explode" }),
    )))
    .unwrap();
    assert_eq!(last_error_code(&out), "invalid_payload");
}

#[test]
fn set_account_scope_persists_and_get_settings_reflects_it() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    block_on(router.handle(request(
        "set_account_scope",
        json!({ "account_id": "acct_archive", "enabled": false }),
    )))
    .unwrap();
    assert_eq!(
        last_ok(&out)["settings"]["account_scopes"]["acct_archive"],
        false
    );

    block_on(router.handle(request("get_settings", json!({})))).unwrap();
    assert_eq!(last_ok(&out)["account_scopes"]["acct_archive"], false);

    // Re-enabling removes the entry (sparse map: absent ⇒ in scope).
    block_on(router.handle(request(
        "set_account_scope",
        json!({ "account_id": "acct_archive", "enabled": true }),
    )))
    .unwrap();
    block_on(router.handle(request("get_settings", json!({})))).unwrap();
    assert!(last_ok(&out)["account_scopes"]
        .get("acct_archive")
        .is_none());

    // A missing enabled is rejected.
    block_on(router.handle(request(
        "set_account_scope",
        json!({ "account_id": "acct_x" }),
    )))
    .unwrap();
    assert_eq!(last_error_code(&out), "invalid_payload");
}

#[test]
fn set_tag_mapping_surfaces_a_new_category_and_clears_on_empty() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    // Map a tag to a BRAND-NEW category key — it must surface as a first-class category.
    block_on(router.handle(request(
        "set_tag_mapping",
        json!({ "tag": "$label1", "category": "VIP" }),
    )))
    .unwrap();
    let settings = last_ok(&out)["settings"].clone();
    assert_eq!(settings["tag_mappings"]["$label1"], "vip", "lower-cased");
    let categories = settings["categories"].as_array().unwrap();
    assert!(
        categories.iter().any(|c| c["key"] == "vip"),
        "the tag-derived category is in the vocabulary"
    );
    // …and is now policy-targetable.
    block_on(router.handle(request(
        "set_category_policy",
        json!({ "category": "vip", "policy": "suggest" }),
    )))
    .unwrap();
    assert_eq!(
        last_ok(&out)["settings"]["category_policies"]["vip"],
        "suggest"
    );

    // An empty category clears the mapping (and the derived category disappears).
    block_on(router.handle(request(
        "set_tag_mapping",
        json!({ "tag": "$label1", "category": "" }),
    )))
    .unwrap();
    block_on(router.handle(request("get_settings", json!({})))).unwrap();
    assert!(last_ok(&out)["tag_mappings"].get("$label1").is_none());
}

#[test]
fn the_consent_gate_keeps_a_body_level_inert_until_consent_is_granted() {
    // The Phase-1a privacy exit: choosing a body-retaining level WITHOUT consent leaves the
    // EFFECTIVE retention at metadata (no body can be stored); granting consent then makes it
    // take effect — and revoking consent clamps it straight back.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    // Level = bodies, but consent not given → effective level stays metadata.
    block_on(router.handle(request(
        "set_settings",
        json!({ "retention_level": "bodies" }),
    )))
    .unwrap();
    block_on(router.handle(request("get_settings", json!({})))).unwrap();
    assert_eq!(
        last_ok(&out)["retention_level"],
        "metadata",
        "a body level is inert without consent"
    );
    assert_eq!(last_ok(&out)["body_consent"], false);

    // Granting consent makes the configured body level take effect.
    block_on(router.handle(request("set_settings", json!({ "body_consent": true })))).unwrap();
    block_on(router.handle(request("get_settings", json!({})))).unwrap();
    assert_eq!(last_ok(&out)["retention_level"], "bodies");
    assert_eq!(last_ok(&out)["body_consent"], true);

    // Revoking consent clamps the effective level straight back to metadata.
    block_on(router.handle(request("set_settings", json!({ "body_consent": false })))).unwrap();
    block_on(router.handle(request("get_settings", json!({})))).unwrap();
    assert_eq!(last_ok(&out)["retention_level"], "metadata");
}

#[test]
fn set_provider_then_set_secret_surface_in_get_settings_without_the_key() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    block_on(router.handle(request(
        "set_provider",
        json!({ "provider_id": "local", "kind": "ollama", "endpoint": "http://127.0.0.1:11434", "set_default": true }),
    )))
    .unwrap();
    let payload = last_ok(&out);
    assert_eq!(payload["settings"]["default_provider"], "local");
    assert_eq!(payload["settings"]["providers"][0]["id"], "local");
    assert_eq!(
        payload["settings"]["providers"][0]["endpoint"],
        "http://127.0.0.1:11434"
    );

    // No key yet → not configured.
    block_on(router.handle(request("get_settings", json!({})))).unwrap();
    assert_eq!(last_ok(&out)["providers"][0]["configured"], false);

    // Writing the key flips configured to true, and the key is never echoed back.
    block_on(router.handle(request(
        "set_secret",
        json!({ "provider_id": "local", "secret": "sk-super-secret" }),
    )))
    .unwrap();
    let stored = last_ok(&out);
    assert_eq!(stored["stored"], true);
    assert!(!serde_json::to_string(&stored)
        .unwrap()
        .contains("sk-super-secret"));

    block_on(router.handle(request("get_settings", json!({})))).unwrap();
    let settings = last_ok(&out);
    assert_eq!(settings["providers"][0]["configured"], true);
    assert!(!serde_json::to_string(&settings)
        .unwrap()
        .contains("sk-super-secret"));
}

#[test]
fn set_secret_refuses_an_empty_value() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());
    block_on(router.handle(request(
        "set_secret",
        json!({ "provider_id": "local", "secret": "" }),
    )))
    .unwrap();
    assert_eq!(last_error_code(&out), "invalid_payload");
}

#[test]
fn set_provider_persists_the_model_and_get_settings_surfaces_it() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    block_on(router.handle(request(
        "set_provider",
        json!({
            "provider_id": "local",
            "kind": "ollama",
            "endpoint": "http://localhost:11434",
            "model": "llama3",
            "set_default": true
        }),
    )))
    .unwrap();
    assert_eq!(last_ok(&out)["settings"]["providers"][0]["model"], "llama3");

    block_on(router.handle(request("get_settings", json!({})))).unwrap();
    assert_eq!(last_ok(&out)["providers"][0]["model"], "llama3");
}

#[test]
fn a_provider_set_in_one_host_session_persists_on_disk_for_the_next() {
    // The regression for "I configured a provider but it still says none". A native-messaging host
    // is short-lived (Thunderbird re-spawns one per port; an MV3 event page drops the port when it
    // suspends), so a provider that lives only in the running process's memory vanishes on the next
    // reconnect — which read as a stuck "Provider: none". With an on-disk config home, the write in
    // session A must still be there when a fresh session B loads the very same file.
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let secrets = dir.path().join("secrets.json");

    // --- Session A: a fresh host, on-disk config home, zero providers — configure one. ---
    let backend_a = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out_a = Arc::new(FakeTransport::new());
    let router_a = build_router(
        &AppConfig::default(),
        &backend_a,
        out_a.clone(),
        secrets.clone(),
        Some(config_path.clone()),
        None,
    )
    .unwrap();
    block_on(router_a.handle(request(
        "set_provider",
        json!({
            "provider_id": "local",
            "kind": "ollama",
            "endpoint": "http://localhost:11434",
            "model": "llama3",
            "set_default": true
        }),
    )))
    .unwrap();
    // The write is now reported as truly persisted (not the old honest-but-useless in-memory case).
    assert_eq!(last_ok(&out_a)["settings"]["persisted"], true);
    assert!(
        config_path.exists(),
        "set_provider must write the on-disk config home"
    );

    // --- Session B: a brand-new host process loads that same config file and sees the provider. ---
    let reloaded = AppConfig::load(&config_path).expect("the persisted config re-loads cleanly");
    let backend_b = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out_b = Arc::new(FakeTransport::new());
    let router_b = build_router(
        &reloaded,
        &backend_b,
        out_b.clone(),
        secrets,
        Some(config_path),
        None,
    )
    .unwrap();
    block_on(router_b.handle(request("get_settings", json!({})))).unwrap();
    let settings = last_ok(&out_b);
    assert_eq!(
        settings["default_provider"], "local",
        "the default provider survives the restart"
    );
    assert_eq!(settings["providers"][0]["id"], "local");
    assert_eq!(settings["providers"][0]["model"], "llama3");
}

#[test]
fn tier2_online_learning_survives_a_host_restart() {
    // The Phase-1a ML-persistence exit criterion as a two-process probe: a spam correction
    // taught in one short-lived host process must still bias the classifier in the *next*
    // process that loads the same weights cache. This proves the persistent Tier-2 model is
    // wired into BOTH the correction path (record_user_action) and the classify path, and that
    // its learning reloads across a restart — not just that a unit-level file round-trips.
    let dir = tempfile::tempdir().unwrap();
    let weights = dir.path().join("tier2_weights.json");

    let classify = |tb: &str| {
        json!({
            "thunderbird_message_id": tb,
            "account_id": "acct_default",
            "folder_id": "inbox",
            "headers": { "from": "sender@example.test", "subject": "hello" },
            "body_retention_allowed": false
        })
    };

    // --- Session A: teach "spam" repeatedly, then drop the process. ---
    {
        let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
        let out = Arc::new(FakeTransport::new());
        let router = build_router(
            &AppConfig::default(),
            &backend,
            out.clone(),
            dir.path().join("secrets_a.json"),
            None,
            Some(weights.clone()),
        )
        .unwrap();
        for _ in 0..12 {
            block_on(router.handle(request(
                "record_user_action",
                json!({ "event_type": "junk_changed", "thunderbird_message_id": "tb_1", "junk": true }),
            )))
            .unwrap();
        }
        assert!(
            weights.exists(),
            "a spam correction must persist the Tier-2 weights cache"
        );
    } // session A's in-memory model is gone — only the on-disk cache remains

    // --- Session B: a brand-new host loads that same cache and classifies hotter than cold. ---
    let backend_b = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out_b = Arc::new(FakeTransport::new());
    let router_b = build_router(
        &AppConfig::default(),
        &backend_b,
        out_b.clone(),
        dir.path().join("secrets_b.json"),
        None,
        Some(weights.clone()),
    )
    .unwrap();
    block_on(router_b.handle(request("classify_message", classify("tb_2")))).unwrap();
    let learned = last_ok(&out_b)["classification"]["spam_score"]
        .as_f64()
        .expect("spam_score is a number");

    // --- Control: a fresh host with NO persisted weights classifies the same message cold. ---
    let backend_c = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out_c = Arc::new(FakeTransport::new());
    let router_c = build_router(
        &AppConfig::default(),
        &backend_c,
        out_c.clone(),
        dir.path().join("secrets_c.json"),
        None,
        None,
    )
    .unwrap();
    block_on(router_c.handle(request("classify_message", classify("tb_2")))).unwrap();
    let cold = last_ok(&out_c)["classification"]["spam_score"]
        .as_f64()
        .expect("spam_score is a number");

    assert!(
        (cold - 0.5).abs() < 1e-6,
        "a cold model is indifferent: {cold}"
    );
    assert!(
        learned > cold + 0.05,
        "the reloaded model kept its spam learning: {learned} vs cold {cold}"
    );
}

#[test]
fn a_correction_records_the_full_feature_vector_not_just_the_domain() {
    // §3.1 as a host-level probe: a spam correction must capture the SAME rich feature vector
    // the classifier saw — not an empty vector — so today's corrections can be mined for richer
    // rules later. The router caches features at classify time and recalls them at correction
    // time (keyed by the internal message id).
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    // Classify a list/bulk message that fails DMARC — a distinctive feature vector.
    block_on(router.handle(request(
        "classify_message",
        json!({
            "thunderbird_message_id": "tb_77",
            "account_id": "acct_default",
            "folder_id": "inbox",
            "headers": {
                "from": "promo@news.test",
                "subject": "deal of the day",
                "list_id": "<newsletter.news.test>",
                "authentication_results": "mx.test; spf=pass; dkim=fail; dmarc=fail"
            },
            "body_retention_allowed": false
        }),
    )))
    .unwrap();

    // The user marks it junk — a spam-axis correction.
    block_on(router.handle(request(
        "record_user_action",
        json!({ "event_type": "junk_changed", "thunderbird_message_id": "tb_77", "junk": true }),
    )))
    .unwrap();

    // The classification_feedback row carries the FULL vector the classifier computed.
    let feedback: Arc<dyn FeedbackRepository<ClassificationFeedback>> =
        Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    let rows = block_on(feedback.query(ClassificationFeedbackQuery {
        message_id: Some(MessageId::from("msg_tb_tb_77")),
        ..Default::default()
    }))
    .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the junk correction recorded exactly one row"
    );
    let sf = &rows[0].salient_features;
    // Rich features are captured — not just the injected sender_domain.
    assert_eq!(sf.get("is_list_mail"), Some(&FeatureValue::Bool(true)));
    assert_eq!(sf.get("auth_fail"), Some(&FeatureValue::Bool(true)));
    assert_eq!(sf.get("dkim_pass"), Some(&FeatureValue::Bool(false)));
    // …and the sender-domain injection still happens.
    assert_eq!(
        sf.get("sender_domain"),
        Some(&FeatureValue::Text("news.test".to_owned()))
    );
}

#[test]
fn a_correction_captures_the_account_from_the_host_cache_for_per_account_induction() {
    // Phase-7 7a per-account scope, host edge: a classification correction must capture the
    // message's account as the `account_id` salient feature, even when the extension omits it on
    // the wire — the host recalls it from the per-message cache it filled at classify time. That is
    // what lets multi-aspect induction later scope a learned rule to the right account.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    // Classify a message on the "work" account — this caches its account_id keyed by message id.
    block_on(router.handle(request(
        "classify_message",
        json!({
            "thunderbird_message_id": "tb_acct",
            "account_id": "work",
            "folder_id": "inbox",
            "headers": { "from": "boss@work.test", "subject": "re: budget" },
            "body_retention_allowed": false
        }),
    )))
    .unwrap();

    // The user marks it junk — WITHOUT sending account_id on the wire. The host must fall back to
    // its cache to recover the account.
    block_on(router.handle(request(
        "record_user_action",
        json!({ "event_type": "junk_changed", "thunderbird_message_id": "tb_acct", "junk": true }),
    )))
    .unwrap();

    let feedback: Arc<dyn FeedbackRepository<ClassificationFeedback>> =
        Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    let rows = block_on(feedback.query(ClassificationFeedbackQuery {
        message_id: Some(MessageId::from("msg_tb_tb_acct")),
        ..Default::default()
    }))
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].salient_features.get("account_id"),
        Some(&FeatureValue::Text("work".to_owned())),
        "the account was recovered from the host cache and folded into the correction"
    );
}

#[test]
fn a_tag_add_and_remove_become_polarised_category_feedback() {
    // §3.4: a tag is a first-class learning signal. Adding a tag records positive category
    // feedback under the tag key; removing it records negative — both carrying the full
    // feature vector (via the classify-time cache), so tags can be mined like any correction.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    block_on(router.handle(request(
        "classify_message",
        json!({
            "thunderbird_message_id": "tb_88",
            "account_id": "acct_default",
            "folder_id": "inbox",
            "headers": { "from": "news@promo.test", "subject": "weekly", "list_id": "<l.promo.test>" },
            "body_retention_allowed": false
        }),
    )))
    .unwrap();

    // The user adds a "newsletters" tag, then removes it.
    block_on(router.handle(request(
        "record_user_action",
        json!({ "event_type": "tag_changed", "thunderbird_message_id": "tb_88", "tag": "newsletters", "added": true }),
    )))
    .unwrap();
    block_on(router.handle(request(
        "record_user_action",
        json!({ "event_type": "tag_changed", "thunderbird_message_id": "tb_88", "tag": "newsletters", "added": false }),
    )))
    .unwrap();

    let feedback: Arc<dyn FeedbackRepository<ClassificationFeedback>> =
        Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    let rows = block_on(feedback.query(ClassificationFeedbackQuery {
        message_id: Some(MessageId::from("msg_tb_tb_88")),
        ..Default::default()
    }))
    .unwrap();
    assert_eq!(rows.len(), 2, "one row per tag change");
    // Newest-first: the remove (negative) then the add (positive).
    assert!(rows.iter().all(|r| r.human_label == "newsletters"));
    let polarities: Vec<_> = rows.iter().map(|r| r.polarity).collect();
    assert!(
        polarities.contains(&FeedbackPolarity::Positive)
            && polarities.contains(&FeedbackPolarity::Negative),
        "an add is positive and a remove is negative: {polarities:?}"
    );
    // The full feature vector rode along (the rich-capture cache), not just the tag.
    assert!(rows
        .iter()
        .any(|r| r.salient_features.get("is_list_mail").is_some()));
}

#[test]
fn salient_signals_explain_a_verdict_and_can_be_marked_wrong() {
    // §3.3 explainability spine as a host probe. After the user teaches the model a handful of
    // spam examples that share a feature, classifying a similar message returns human-readable,
    // correctable salient signals — the *actual* top contributions, not post-hoc IDs — and
    // rejecting one records a negative classification_feedback row keyed on that signal.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    // Teach: six DISTINCT senders, all failing authentication, all marked junk. Only the shared
    // auth features cross the per-feature confidence floor (each varied sender one-hot stays
    // below it), so the learned signal is the authentication failure, not any one sender.
    for i in 0..6 {
        let tb = format!("tb_train_{i}");
        block_on(router.handle(request(
            "classify_message",
            json!({
                "thunderbird_message_id": tb,
                "account_id": "acct_default",
                "folder_id": "inbox",
                "headers": {
                    "from": format!("sender{i}@spam{i}.test"),
                    "subject": "hello there",
                    "authentication_results": "mx.test; spf=fail; dkim=fail; dmarc=fail"
                },
                "body_retention_allowed": false
            }),
        )))
        .unwrap();
        block_on(router.handle(request(
            "record_user_action",
            json!({ "event_type": "junk_changed", "thunderbird_message_id": tb, "junk": true }),
        )))
        .unwrap();
    }

    // Now classify a fresh, similar message and read the explanation off the verdict.
    block_on(router.handle(request(
        "classify_message",
        json!({
            "thunderbird_message_id": "tb_target",
            "account_id": "acct_default",
            "folder_id": "inbox",
            "headers": {
                "from": "newguy@fresh.test",
                "subject": "hello there",
                "authentication_results": "mx.test; spf=fail; dkim=fail; dmarc=fail"
            },
            "body_retention_allowed": false
        }),
    )))
    .unwrap();
    let resp = last_ok(&out);
    let signals = resp["classification"]["salient_signals"]
        .as_array()
        .expect("the verdict carries salient_signals");
    assert!(
        !signals.is_empty(),
        "the verdict carries readable reasons: {resp:#}"
    );
    // The reasons are human labels (never raw snake_case ids), and at least one is correctable.
    let correctable: Vec<&Value> = signals
        .iter()
        .filter(|s| s["correctable"] == json!(true))
        .collect();
    assert!(
        !correctable.is_empty(),
        "at least one signal is correctable: {signals:#?}"
    );
    let chip = correctable[0];
    assert_eq!(chip["source"], "deterministic_feature");
    let label = chip["label"].as_str().unwrap();
    assert!(
        label.chars().next().is_some_and(char::is_uppercase),
        "the label is a readable phrase, not a raw id: {label:?}"
    );
    let signal_id = chip["id"].as_str().unwrap().to_owned();

    // The user clicks "this reason is wrong" on that chip.
    block_on(router.handle(request(
        "record_user_action",
        json!({
            "event_type": "signal_marked_wrong",
            "thunderbird_message_id": "tb_target",
            "signal_id": signal_id,
            "prior_label": "needs_review"
        }),
    )))
    .unwrap();

    // A negative classification_feedback row is recorded, keyed on the rejected signal, carrying
    // the full feature vector (so a feature the user keeps rejecting is minable as evidence).
    let feedback: Arc<dyn FeedbackRepository<ClassificationFeedback>> =
        Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    let rows = block_on(feedback.query(ClassificationFeedbackQuery {
        message_id: Some(MessageId::from("msg_tb_tb_target")),
        ..Default::default()
    }))
    .unwrap();
    assert_eq!(rows.len(), 1, "exactly one rejected-signal row");
    assert_eq!(
        rows[0].human_reason_code.as_deref(),
        Some(signal_id.as_str())
    );
    assert_eq!(rows[0].polarity, FeedbackPolarity::Negative);
    assert!(
        rows[0].salient_features.get("auth_fail").is_some(),
        "the full feature vector rode along via the classify-time cache"
    );
}

#[test]
fn the_classify_payload_carries_an_inform_only_safety_block() {
    // §3.6 safety surface as a host probe: a message that fails authentication and carries an
    // executable attachment surfaces inform-only Safety findings on the verdict — without taking
    // any action (the findings are not policy; nothing is auto-applied off them).
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    block_on(router.handle(request(
        "classify_message",
        json!({
            "thunderbird_message_id": "tb_phish",
            "account_id": "acct_default",
            "folder_id": "inbox",
            "headers": {
                "from": "security@paypa1.test",
                "subject": "Your account is suspended — verify now",
                "authentication_results": "mx.test; spf=fail; dkim=fail; dmarc=fail"
            },
            "attachments": [
                { "filename": "invoice.pdf.exe", "content_type": "application/octet-stream", "size_bytes": 2048 }
            ],
            "body_retention_allowed": false
        }),
    )))
    .unwrap();

    let resp = last_ok(&out);
    // A cold-start verdict (untrained model) is honestly low-confidence and needs review — the
    // panel renders the calibrated band, never a fabricated certainty.
    assert_eq!(resp["classification"]["needs_review"], true);
    assert_eq!(resp["classification"]["confidence_band"], "low");
    let safety = resp["classification"]["safety_findings"]
        .as_array()
        .expect("the verdict carries a Safety block");
    let ids: Vec<&str> = safety.iter().filter_map(|f| f["id"].as_str()).collect();
    assert!(ids.contains(&"executable_attachment"), "got {ids:?}");
    assert!(ids.contains(&"auth_failure"), "got {ids:?}");
    // The dangerous attachment is a Danger-severity finding that names the file.
    let exe = safety
        .iter()
        .find(|f| f["id"] == "executable_attachment")
        .unwrap();
    assert_eq!(exe["severity"], "danger");
    assert!(exe["detail"].as_str().unwrap().contains("invoice.pdf.exe"));
    // Inform-only: the Safety block changes nothing the host does — no action was auto-applied.
    assert!(
        resp["suggested_actions"]
            .as_array()
            .is_none_or(|a| a.iter().all(|x| x["apply_state"] != "auto_applied")),
        "safety findings never drive an auto-applied action"
    );
}

#[test]
fn the_classify_payload_exposes_a_one_click_unsubscribe_affordance() {
    // §Phase-1b: a List-Unsubscribe header becomes a typed affordance on the verdict so the panel
    // can offer one-click unsubscribe (a pre-addressed compose, or an RFC 8058 POST).
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    block_on(router.handle(request(
        "classify_message",
        json!({
            "thunderbird_message_id": "tb_news",
            "account_id": "acct_default",
            "folder_id": "inbox",
            "headers": {
                "from": "news@list.test",
                "subject": "Weekly digest",
                "list_id": "<weekly.list.test>",
                "list_unsubscribe": "<mailto:unsub@list.test?subject=unsubscribe>, <https://list.test/u?id=9>",
                "list_unsubscribe_post": "List-Unsubscribe=One-Click"
            },
            "body_retention_allowed": false
        }),
    )))
    .unwrap();

    let resp = last_ok(&out);
    let unsub = &resp["unsubscribe"];
    assert_eq!(unsub["mailto"]["to"], "unsub@list.test");
    assert_eq!(unsub["mailto"]["subject"], "unsubscribe");
    assert_eq!(unsub["http_url"], "https://list.test/u?id=9");
    assert_eq!(
        unsub["one_click"], true,
        "https + the RFC 8058 marker ⇒ one-click"
    );

    // A plain message carries no unsubscribe affordance.
    let out2 = Arc::new(FakeTransport::new());
    let router2 = router_over(&backend, out2.clone());
    block_on(router2.handle(request(
        "classify_message",
        json!({
            "thunderbird_message_id": "tb_plain",
            "account_id": "acct_default",
            "folder_id": "inbox",
            "headers": { "from": "a@b.test", "subject": "Hi" },
            "body_retention_allowed": false
        }),
    )))
    .unwrap();
    assert!(
        last_ok(&out2)["unsubscribe"].is_null(),
        "no header ⇒ no affordance"
    );
}

fn filing_feedback_row(domain: &str, folder: &str) -> FilingFeedbackRow {
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

/// The most recent notification of `type_` the host emitted.
fn last_notification(out: &FakeTransport, type_: &str) -> Value {
    out.sent_frames()
        .into_iter()
        .rev()
        .find_map(|f| match f {
            Frame::Notification {
                type_: t, payload, ..
            } if t == type_ => Some(payload),
            _ => None,
        })
        .unwrap_or_else(|| panic!("a {type_} notification was emitted"))
}

/// The payloads of every `proposal_ready` notification the host has emitted so far.
fn proposal_ready_notifications(out: &FakeTransport) -> Vec<Value> {
    out.sent_frames()
        .into_iter()
        .filter_map(|f| match f {
            Frame::Notification { type_, payload, .. } if type_ == "proposal_ready" => {
                Some(payload)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn generate_proposals_mines_recurring_filings_and_emits_proposal_ready_once() {
    // The deterministic learning loop, end to end through the real composition root: three
    // same-domain → same-folder moves cross the bar, so a proposal is mined, persisted to the
    // review queue, and announced via `proposal_ready` — all with ZERO providers configured.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    let feedback: Arc<dyn FeedbackRepository<FilingFeedback>> =
        Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    for _ in 0..3 {
        block_on(feedback.append(filing_feedback_row("stripe.com", "Receipts"))).unwrap();
    }

    block_on(router.generate_proposals()).unwrap();

    // One `proposal_ready`, carrying exactly the fields the extension's handler reads.
    let ready = proposal_ready_notifications(&out);
    assert_eq!(
        ready.len(),
        1,
        "one proposal_ready for the one crossed cluster"
    );
    assert_eq!(ready[0]["title"], "File stripe.com mail to Receipts");
    assert_eq!(ready[0]["risk_level"], "low");
    assert!(ready[0]["proposal_id"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));

    // It is now in the human review queue — pending_review, never auto-activated.
    let proposals = SqliteProposalRepository::new(backend.clone());
    let pending = block_on(proposals.list_by_status(ProposalStatus::PendingReview)).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].recommended_status, RuleStatus::ShadowMode);

    // Idempotent: a second pass over the SAME feedback emits no new proposal and no new frame —
    // exactly what makes the catch-up + per-tick wiring safe.
    block_on(router.generate_proposals()).unwrap();
    assert_eq!(
        proposal_ready_notifications(&out).len(),
        1,
        "no duplicate proposal_ready on a second pass"
    );
    assert_eq!(
        block_on(proposals.list_by_status(ProposalStatus::PendingReview))
            .unwrap()
            .len(),
        1,
        "no duplicate proposal persisted"
    );
}

// --- list_models (provider model discovery) ------------------------------------------------
//
// Driven over the REAL `build_router` (a real ureq `StdHttpClient`) against a localhost stub that
// returns an Ollama-shaped catalog — exercising the whole glue (payload extraction → GET → the
// provider-specific parse) end-to-end, with no external network.

/// A one-shot localhost HTTP server returning `response` to the first request (any method/path).
/// Returns the base URL to use as a provider endpoint. Drains the request before responding so
/// closing the socket sends FIN, not RST (a half-read socket would surface as a client read error).
fn stub_endpoint(response: Vec<u8>) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(200)));
            let mut buf = [0u8; 8192];
            while let Ok(n) = sock.read(&mut buf) {
                if n == 0 {
                    break; // peer closed
                }
                // else: keep draining until the read timeout fires (request fully received)
            }
            let _ = sock.write_all(&response);
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// A `200 OK` HTTP response carrying `json_body`.
fn ok_body(json_body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json_body}",
        json_body.len()
    )
    .into_bytes()
}

/// Like [`stub_endpoint`], but also **captures the raw bytes of the request** it receives, so a
/// test can assert what headers (e.g. `Authorization`) the host did — or did not — send. Returns
/// the base URL plus the shared capture buffer. The request is fully received before the canned
/// response is written, so the buffer is populated by the time a blocking client call returns.
fn capturing_stub_endpoint(response: Vec<u8>) -> (String, Arc<std::sync::Mutex<Vec<u8>>>) {
    use std::io::{Read, Write};
    use std::sync::Mutex;
    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(200)));
            let mut buf = [0u8; 8192];
            while let Ok(n) = sock.read(&mut buf) {
                if n == 0 {
                    break; // peer closed
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
            let _ = sock.write_all(&response);
        }
    });
    (format!("http://127.0.0.1:{port}"), captured)
}

#[test]
fn list_models_lists_an_ollama_catalog_over_the_real_transport() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());
    let endpoint = stub_endpoint(ok_body(
        r#"{"models":[{"name":"llama3:latest"},{"name":"qwen2.5:7b"}]}"#,
    ));

    block_on(router.handle(request(
        "list_models",
        json!({ "kind": "ollama", "endpoint": endpoint }),
    )))
    .unwrap();

    let payload = one_ok_response(&out);
    assert_eq!(payload["kind"], "ollama");
    let names: Vec<&str> = payload["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(names, vec!["llama3:latest", "qwen2.5:7b"]);
}

#[test]
fn list_models_without_kind_or_endpoint_is_an_invalid_payload() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    block_on(router.handle(request("list_models", json!({})))).unwrap();

    assert_eq!(one_error_code(&out), "invalid_payload");
}

#[test]
fn list_models_without_the_admin_suite_is_admin_not_configured() {
    let out = Arc::new(FakeTransport::new());
    let bare = bare_router(out.clone());

    block_on(bare.handle(request(
        "list_models",
        json!({ "kind": "ollama", "endpoint": "http://localhost:11434" }),
    )))
    .unwrap();

    assert_eq!(one_error_code(&out), "admin_not_configured");
}

#[test]
fn list_models_resolves_kind_and_endpoint_from_a_saved_provider() {
    // The provider-card "refresh models" path: the request carries only a provider_id, so the host
    // must fall back to that saved provider's kind + endpoint (and would attach its stored key).
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());
    let endpoint = stub_endpoint(ok_body(r#"{"models":[{"name":"saved-model"}]}"#));

    // Save a provider with that endpoint, then ask for its models by id ALONE.
    block_on(router.handle(request(
        "set_provider",
        json!({ "provider_id": "local", "kind": "ollama", "endpoint": endpoint }),
    )))
    .unwrap();
    block_on(router.handle(request("list_models", json!({ "provider_id": "local" })))).unwrap();

    // last_ok reads the latest frame — the list_models response, not the set_provider one.
    let payload = last_ok(&out);
    assert_eq!(payload["kind"], "ollama");
    let names: Vec<&str> = payload["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(names, vec!["saved-model"]);
}

#[test]
fn list_models_never_sends_the_saved_key_to_an_overridden_endpoint() {
    // SSRF / key-exfiltration guard: a request pins a saved `provider_id` (whose stored key the
    // host would attach) but OVERRIDES the endpoint with an attacker URL. The saved key MUST NOT
    // be sent there — discovery still runs, just unauthenticated.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    // A saved cloud provider (its kind attaches a Bearer key) with a stored key. Its own endpoint
    // is never connected to in this test — it just has to exist in config.
    block_on(router.handle(request(
        "set_provider",
        json!({ "provider_id": "cloud", "kind": "openai_compatible", "endpoint": "http://saved.invalid/v1", "model": "m" }),
    )))
    .unwrap();
    block_on(router.handle(request(
        "set_secret",
        json!({ "provider_id": "cloud", "secret": "sk-leak-me" }),
    )))
    .unwrap();

    // Probe an ATTACKER endpoint while pinning the saved provider_id.
    let (attacker, captured) = capturing_stub_endpoint(ok_body(r#"{"data":[]}"#));
    block_on(router.handle(request(
        "list_models",
        json!({ "provider_id": "cloud", "endpoint": attacker }),
    )))
    .unwrap();

    let seen = String::from_utf8_lossy(&captured.lock().unwrap()).to_ascii_lowercase();
    assert!(
        !seen.contains("authorization"),
        "the saved key must never be sent to an overridden endpoint; request was:\n{seen}"
    );
    assert!(
        !seen.contains("sk-leak-me"),
        "the saved key value leaked to the attacker endpoint:\n{seen}"
    );
}

#[test]
fn list_models_sends_the_saved_key_to_the_providers_own_endpoint() {
    // The companion to the SSRF guard: discovery against the provider's OWN saved endpoint still
    // carries its stored key, so authenticated cloud catalogs keep working.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    let (saved, captured) = capturing_stub_endpoint(ok_body(r#"{"data":[{"id":"m"}]}"#));
    block_on(router.handle(request(
        "set_provider",
        json!({ "provider_id": "cloud", "kind": "openai_compatible", "endpoint": saved, "model": "m" }),
    )))
    .unwrap();
    block_on(router.handle(request(
        "set_secret",
        json!({ "provider_id": "cloud", "secret": "sk-keep" }),
    )))
    .unwrap();

    // List by provider_id ALONE — no override, so the resolved endpoint IS the saved one.
    block_on(router.handle(request("list_models", json!({ "provider_id": "cloud" })))).unwrap();

    let seen = String::from_utf8_lossy(&captured.lock().unwrap()).to_ascii_lowercase();
    assert!(
        seen.contains("authorization: bearer sk-keep"),
        "the stored key must authenticate discovery against the provider's own endpoint:\n{seen}"
    );
}

#[test]
fn provider_status_reports_unconfigured_then_available_after_a_complete_default() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    // Nothing configured → not configured, not available, no default.
    block_on(router.handle(request("provider_status", json!({})))).unwrap();
    let before = last_ok(&out);
    assert_eq!(before["configured"], false);
    assert_eq!(before["available"], false);
    assert!(before["default_provider"].is_null());

    // Save a COMPLETE ollama provider and make it the default.
    block_on(router.handle(request(
        "set_provider",
        json!({
            "provider_id": "local",
            "kind": "ollama",
            "endpoint": "http://localhost:11434",
            "model": "llama3",
            "set_default": true
        }),
    )))
    .unwrap();

    block_on(router.handle(request("provider_status", json!({})))).unwrap();
    let after = last_ok(&out);
    assert_eq!(after["configured"], true);
    // `available` comes from build_provider itself — a complete ollama config resolves.
    assert_eq!(after["available"], true);
    assert_eq!(after["default_provider"], "local");
    assert_eq!(after["provider"]["kind"], "ollama");
    assert_eq!(after["provider"]["model"], "llama3");
    // Local adapters take no key; the panel must not nag for one.
    assert_eq!(after["provider"]["accepts_key"], false);
    assert_eq!(after["provider"]["secret_set"], false);
}

#[test]
fn provider_status_is_configured_but_unavailable_when_the_default_is_incomplete() {
    // A named default whose config can't build (ollama with no model) is the honest "configured
    // but the draft path would still get nothing" case — `available` must be false, proving it is
    // computed by build_provider, not faked from `default_provider.is_some()`.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    block_on(router.handle(request(
        "set_provider",
        json!({ "provider_id": "local", "kind": "ollama", "endpoint": "http://localhost:11434", "set_default": true }),
    )))
    .unwrap();

    block_on(router.handle(request("provider_status", json!({})))).unwrap();
    let status = last_ok(&out);
    assert_eq!(status["configured"], true);
    assert_eq!(
        status["available"], false,
        "an incomplete default must not read as available"
    );
}

#[test]
fn test_provider_reports_reachable_over_a_live_stub() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());
    let endpoint = stub_endpoint(ok_body(r#"{"models":[{"name":"llama3:latest"}]}"#));

    block_on(router.handle(request(
        "test_provider",
        json!({ "kind": "ollama", "endpoint": endpoint }),
    )))
    .unwrap();

    let payload = one_ok_response(&out);
    assert_eq!(payload["reachable"], true);
    assert_eq!(payload["model_count"], 1);
}

#[test]
fn test_provider_reports_unreachable_as_data_not_an_error() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    // Nothing is listening on port 1 → connection refused → a *successful* test reporting
    // reachable:false on an `ok` frame (not a protocol error).
    block_on(router.handle(request(
        "test_provider",
        json!({ "kind": "ollama", "endpoint": "http://127.0.0.1:1" }),
    )))
    .unwrap();

    let payload = one_ok_response(&out);
    assert_eq!(payload["reachable"], false);
    assert!(
        payload["error"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false),
        "an unreachable probe must carry the transport error: {payload}"
    );
}

// --- Phase 3 exit: a "No thanks" reply over the REAL Ollama transport ----------------------
//
// Drives draft_reply through the REAL build_router with a configured Ollama provider pointed at a
// localhost stub returning an Ollama-shaped completion. Proves the whole compose vertical end to
// end with no live model: the real provider transport draws the draft, the host runs the
// model-free commitments guard over the body, and the response carries the rationale + the typed
// guard + the review-required flag. (The flagged-with-spans guard case is the host_router guard
// probe; reply-from-correct-identity + the edit-divergence send hook are extension glue, reviewed
// in drafts.js/background.js with the host's `draft_diverged` audit route covered by host_router.)
#[test]
fn draft_reply_over_a_real_ollama_stub_returns_a_guarded_review_required_draft() {
    use mailmate_native_host::config::{AiSettings, ProviderSettings};
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());

    // The model's output, in Ollama's /api/chat envelope: message.content is the draft JSON string.
    let draft_json = json!({
        "subject": "Re: Your proposal",
        "body": "Hi,\n\nThanks for the offer — no thanks for now. I'll reach out if that changes.\n\nBest,",
        "safety_notes": [],
        "rationale": "A polite decline with no commitments, matching how you've replied before."
    })
    .to_string();
    let envelope = json!({ "message": { "content": draft_json } }).to_string();
    let endpoint = stub_endpoint(ok_body(&envelope));

    let config = AppConfig {
        ai: AiSettings {
            default_provider: Some("local".to_owned()),
            providers: vec![ProviderSettings {
                id: "local".to_owned(),
                kind: "ollama".to_owned(),
                endpoint: Some(endpoint),
                model: Some("llama3".to_owned()),
            }],
        },
        ..AppConfig::default()
    };
    let secrets = std::env::temp_dir().join("mailmate-phase3-exit-secrets.json");
    let router = build_router(&config, &backend, out.clone(), secrets, None, None).unwrap();

    block_on(router.handle(request(
        "draft_reply",
        json!({
            "subject": "Your proposal",
            "counterparty": "vendor@example.test",
            "excerpt": "Are you interested in our offer?",
            "user_instruction": "Politely decline.",
            "forbidden_commitments": ["dates", "prices"]
        }),
    )))
    .unwrap();

    let payload = one_ok_response(&out);
    assert_eq!(
        payload["requires_human_review"], true,
        "a draft is never auto-sent"
    );
    assert!(
        payload["body"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("no thanks"),
        "the decline drawn over the real transport reached the host: {}",
        payload["body"]
    );
    assert!(
        payload["rationale"]
            .as_str()
            .unwrap()
            .contains("polite decline"),
        "the rationale rode through: {}",
        payload["rationale"]
    );
    // The model-free guard ran on the host — a clean decline is all-clear (no fabricated flags).
    assert!(
        payload["commitments"]["findings"]
            .as_array()
            .unwrap()
            .is_empty(),
        "a clean decline is all-clear: {}",
        payload["commitments"]
    );
}

#[test]
fn set_provider_hot_swaps_the_live_drafter_with_no_restart() {
    // P2 regression: the live provider is rebuilt + hot-swapped in place after a `set_provider`
    // write. A router that STARTED with no provider (so `draft_reply` degrades to `draft_failed`)
    // must draft successfully once a provider is configured from Settings — over the SAME router
    // instance, proving the drafter's backing adapter swapped without a host restart.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone()); // default config → UnavailableProvider backing

    // 1) Cold: nothing configured → draft degrades (no network hit, never a fabricated draft).
    block_on(router.handle(request("draft_reply", draft_request_payload()))).unwrap();
    assert_eq!(
        last_error_code(&out),
        "draft_failed",
        "with no provider the drafter must degrade, not fabricate"
    );

    // 2) Configure a working Ollama provider from "Settings", pointed at a stub, as the default.
    let draft_json = json!({
        "subject": "Re: Your proposal",
        "body": "Hi,\n\nThanks, but no thanks for now.\n\nBest,",
        "safety_notes": [],
        "rationale": "A polite decline with no commitments."
    })
    .to_string();
    let envelope = json!({ "message": { "content": draft_json } }).to_string();
    let endpoint = stub_endpoint(ok_body(&envelope));
    block_on(router.handle(request(
        "set_provider",
        json!({ "provider_id": "local", "kind": "ollama", "endpoint": endpoint, "model": "llama3", "set_default": true }),
    )))
    .unwrap();

    // 3) Warm: the SAME router now drafts over the freshly-swapped provider.
    block_on(router.handle(request("draft_reply", draft_request_payload()))).unwrap();
    let warm = last_ok(&out);
    assert_eq!(
        warm["requires_human_review"], true,
        "a draft is never auto-sent"
    );
    assert!(
        warm["body"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("no thanks"),
        "the draft drawn over the hot-swapped provider reached the host: {}",
        warm["body"]
    );
}

/// A `draft_reply` request payload (shared by the hot-swap regression's cold + warm calls).
fn draft_request_payload() -> Value {
    json!({
        "subject": "Your proposal",
        "counterparty": "vendor@example.test",
        "excerpt": "Are you interested in our offer?",
        "user_instruction": "Politely decline.",
        "forbidden_commitments": ["dates", "prices"]
    })
}

// --- The Golden Path (Phase 2 exit) --------------------------------------------------------
//
// One scripted vertical through the REAL composition root, in a SINGLE host process:
// correction → mined+back-tested proposal → approve & activate → auto-applied move → working
// Undo. This is the probe that turns the previously-dead `back_test` gate, the previously-absent
// activation transition, and the previously-missing hot-reload all load-bearing at once — and it
// would go RED if any of the three regressed (an inert gate over-proposes; no activation leaves
// the rule shadow; no reload leaves an activated rule silently not firing until restart).

/// A `record_user_action{message_moved}` frame: the user filed `tb` (from `domain`) into `folder`.
fn move_action(tb: &str, domain: &str, folder: &str) -> Frame {
    request(
        "record_user_action",
        json!({
            "event_type": "message_moved",
            "thunderbird_message_id": tb,
            "from_folder_id": "inbox",
            "to_folder_id": folder,
            "user_initiated": true,
            "sender_domain": domain,
        }),
    )
}

/// A `new_mail` background arrival from `from` sitting in `inbox`.
fn new_mail_from(tb: &str, from: &str) -> Frame {
    request(
        "new_mail",
        json!({
            "thunderbird_message_id": tb,
            "account_id": "acct_default",
            "folder_id": "inbox",
            "headers": { "from": from, "subject": "Your receipt" },
            "body_text": "thanks for your payment",
            "body_retention_allowed": false,
        }),
    )
}

/// A `new_mail` arrival on a SPECIFIC account (so per-account scope is observable).
fn new_mail_on(tb: &str, account: &str) -> Frame {
    request(
        "new_mail",
        json!({
            "thunderbird_message_id": tb,
            "account_id": account,
            "folder_id": "inbox",
            "headers": { "from": "someone@example.test", "subject": "Newsletter" },
            "body_text": "this week's news",
            "body_retention_allowed": false,
        }),
    )
}

/// A fakes-backed router with an admin carrying `config` and a CONTROLLED classification
/// (`labels`) and plan (`planned`). The real engine's labels are not ours to choose, so the
/// per-category and per-account suppression is probed over fakes; the admin's proposals, secret,
/// and http are present but never exercised by the `new_mail` enforcement path.
fn triage_router(
    config: AppConfig,
    labels: &[&str],
    planned: Vec<mailmate_common::action::ProposedAction>,
) -> (
    HostRouter,
    Arc<FakeTransport>,
    Arc<mailmate_test_support::fakes::FakeMailClient>,
) {
    use mailmate_core::Ports;
    use mailmate_native_host::http_client::StdHttpClient;
    use mailmate_native_host::router::AdminSuite;
    use mailmate_test_support::fakes::{
        FakeActionPlanner, FakeAuditRepository, FakeClassificationEngine, FakeClock,
        FakeLearningEngine, FakeMailClient, FakePolicyGuard, FakeProposalReview, FakeReplyDrafter,
        FakeRuleCurator, FakeSecretStore, FakeTier2Classifier, FakeTrainingPipeline,
        StubFeatureExtractor,
    };
    use std::sync::Mutex;

    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let mail = Arc::new(FakeMailClient::new());

    let mut classification = neutral();
    classification.labels = labels.iter().map(|s| (*s).to_owned()).collect();

    let ports = Ports {
        mail_client: mail.clone(),
        transport: out.clone(),
        clock: Arc::new(FakeClock::new(Timestamp::now())),
        secret_store: Arc::new(FakeSecretStore::new()),
        feature_extractor: Arc::new(StubFeatureExtractor),
        tier2: Arc::new(FakeTier2Classifier::new()),
        classification_engine: Arc::new(FakeClassificationEngine::returning(classification)),
        action_planner: Arc::new(FakeActionPlanner::returning(planned)),
        policy_guard: Arc::new(FakePolicyGuard::new()),
        learning_engine: Arc::new(FakeLearningEngine::new()),
        rule_curator: Arc::new(FakeRuleCurator::new()),
        proposal_review: Arc::new(FakeProposalReview::new()),
        reply_drafter: Arc::new(FakeReplyDrafter::new()),
        training_pipeline: Arc::new(FakeTrainingPipeline::new()),
    };
    let admin = AdminSuite {
        proposals: Arc::new(SqliteProposalRepository::new(backend.clone())),
        config: Arc::new(Mutex::new(config)),
        config_path: None,
        secret_store: Arc::new(FakeSecretStore::new()),
        http: Arc::new(StdHttpClient::new()),
        // This admin-only test router has no live provider graph to swap; settings writes still
        // take effect on the config/secret store, they just skip the (absent) rebuild.
        live_provider: None,
    };
    let router = HostRouter::from_ports(&ports, Arc::new(FakeAuditRepository::new()), out.clone())
        .with_admin(admin);
    (router, out, mail)
}

/// A planned tag the policy guard allows (so it lands in `allowed_actions` and would auto-apply
/// under `Auto`). The message id is arbitrary — the fake mail client applies whatever it is given.
fn tag_plan() -> Vec<mailmate_common::action::ProposedAction> {
    vec![mailmate_common::action::ProposedAction::Tag {
        message_id: MessageId::from("msg_x"),
        tag: "Newsletters".to_owned(),
    }]
}

#[test]
fn a_category_set_to_off_silences_its_suggestions_the_phase5_exit() {
    // The Phase-5 exit: disabling a category stops its suggestions. A message classified
    // `newsletters` with a tag the guard would allow…
    let mut cfg = AppConfig::default();
    cfg.triage
        .category_policies
        .insert("newsletters".to_owned(), CategoryPolicy::Off);
    let (router, out, mail) = triage_router(cfg, &["newsletters"], tag_plan());

    block_on(router.handle(new_mail_on("tb_1", "acct_default"))).unwrap();

    // Nothing applied, and — crucially — nothing surfaced to act on.
    assert!(
        mail.applied_actions().is_empty(),
        "Off auto-applies nothing"
    );
    let ntf = last_notification(&out, "classification_ready");
    assert_eq!(
        ntf["applied_actions"].as_array().unwrap().len(),
        0,
        "no applied actions"
    );
    assert_eq!(
        ntf["review_required_actions"].as_array().unwrap().len(),
        0,
        "Off stops the suggestions too"
    );
    // The verdict itself still rides along (the category is silenced, not unclassified).
    assert!(ntf["classification"].is_object());
}

#[test]
fn a_category_set_to_suggest_surfaces_the_action_but_never_auto_applies() {
    let mut cfg = AppConfig::default();
    cfg.triage
        .category_policies
        .insert("newsletters".to_owned(), CategoryPolicy::Suggest);
    let (router, out, mail) = triage_router(cfg, &["newsletters"], tag_plan());

    block_on(router.handle(new_mail_on("tb_1", "acct_default"))).unwrap();

    assert!(
        mail.applied_actions().is_empty(),
        "Suggest auto-applies nothing"
    );
    let ntf = last_notification(&out, "classification_ready");
    assert_eq!(ntf["applied_actions"].as_array().unwrap().len(), 0);
    // The would-be-auto action is DEMOTED to a suggestion (not silently dropped).
    let review = ntf["review_required_actions"].as_array().unwrap();
    assert_eq!(review.len(), 1, "the action surfaces as a suggestion");
    assert_eq!(review[0]["kind"], "tag");
    assert_eq!(review[0]["policy_outcome"], "requires_review");
    assert_eq!(
        review[0]["apply_state"], "suggest",
        "in the loop, not applied"
    );
}

#[test]
fn the_default_auto_policy_still_auto_applies() {
    // The control: with no policy set, the allowed action auto-applies as before.
    let (router, out, mail) = triage_router(AppConfig::default(), &["newsletters"], tag_plan());
    block_on(router.handle(new_mail_on("tb_1", "acct_default"))).unwrap();
    assert_eq!(mail.applied_actions().len(), 1, "Auto applies the action");
    let ntf = last_notification(&out, "classification_ready");
    assert_eq!(ntf["applied_actions"][0]["kind"], "tag");
}

#[test]
fn a_policy_matches_a_classification_label_case_insensitively() {
    // Category keys are lower-cased, but a classification label is free-form — a policy on
    // `newsletters` must still silence a message the engine labelled `Newsletters`.
    let mut cfg = AppConfig::default();
    cfg.triage
        .category_policies
        .insert("newsletters".to_owned(), CategoryPolicy::Off);
    let (router, out, mail) = triage_router(cfg, &["Newsletters"], tag_plan());

    block_on(router.handle(new_mail_on("tb_1", "acct_default"))).unwrap();
    assert!(
        mail.applied_actions().is_empty(),
        "a capitalized label still matches the lower-cased policy key"
    );
    let ntf = last_notification(&out, "classification_ready");
    assert_eq!(ntf["review_required_actions"].as_array().unwrap().len(), 0);
}

#[test]
fn the_strictest_policy_wins_across_multiple_labels() {
    // A message carrying several labels takes the MOST RESTRICTIVE of their policies: one `Off`
    // label silences it even though another label is `Auto`.
    let mut cfg = AppConfig::default();
    cfg.triage
        .category_policies
        .insert("newsletters".to_owned(), CategoryPolicy::Off);
    // promotions stays Auto (no entry).
    let (router, out, mail) = triage_router(cfg, &["promotions", "newsletters"], tag_plan());

    block_on(router.handle(new_mail_on("tb_1", "acct_default"))).unwrap();
    assert!(
        mail.applied_actions().is_empty(),
        "the Off label silences the arrival despite the Auto label"
    );
    let ntf = last_notification(&out, "classification_ready");
    assert_eq!(ntf["applied_actions"].as_array().unwrap().len(), 0);
    assert_eq!(ntf["review_required_actions"].as_array().unwrap().len(), 0);
}

#[test]
fn removing_a_tag_mapping_prunes_its_orphaned_category_policy() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    // Introduce category "vip" via a tag, then set a policy on it.
    block_on(router.handle(request(
        "set_tag_mapping",
        json!({ "tag": "$label1", "category": "vip" }),
    )))
    .unwrap();
    block_on(router.handle(request(
        "set_category_policy",
        json!({ "category": "vip", "policy": "off" }),
    )))
    .unwrap();
    assert_eq!(last_ok(&out)["settings"]["category_policies"]["vip"], "off");

    // Remove the mapping → the orphaned vip policy must be pruned (cannot silently reactivate).
    block_on(router.handle(request(
        "set_tag_mapping",
        json!({ "tag": "$label1", "category": "" }),
    )))
    .unwrap();
    block_on(router.handle(request("get_settings", json!({})))).unwrap();
    assert!(
        last_ok(&out)["category_policies"].get("vip").is_none(),
        "the orphaned policy was pruned"
    );
}

#[test]
fn an_out_of_scope_account_is_silenced_regardless_of_category() {
    // Account scope is a hard silence: even an Auto category on an out-of-scope account does
    // nothing and surfaces nothing.
    let mut cfg = AppConfig::default();
    cfg.triage
        .account_scopes
        .insert("acct_archive".to_owned(), false);
    let (router, out, mail) = triage_router(cfg, &["newsletters"], tag_plan());

    block_on(router.handle(new_mail_on("tb_1", "acct_archive"))).unwrap();
    assert!(mail.applied_actions().is_empty());
    let ntf = last_notification(&out, "classification_ready");
    assert_eq!(ntf["applied_actions"].as_array().unwrap().len(), 0);
    assert_eq!(ntf["review_required_actions"].as_array().unwrap().len(), 0);

    // …but an in-scope account on the same router still auto-applies.
    block_on(router.handle(new_mail_on("tb_2", "acct_default"))).unwrap();
    assert_eq!(
        mail.applied_actions().len(),
        1,
        "in-scope account is normal"
    );
}

#[test]
fn pausing_demotes_auto_actions_to_suggestions_rather_than_dropping_them() {
    // The pause↔policy unification: a paused host classifies and SURFACES every action as a
    // suggestion (matching the UI promise "everything becomes a suggestion"), auto-applying none —
    // the pre-Phase-5 pause silently dropped allowed actions from the notification.
    let cfg = AppConfig {
        paused: true,
        ..AppConfig::default()
    };
    let (router, out, mail) = triage_router(cfg, &["newsletters"], tag_plan());

    block_on(router.handle(new_mail_on("tb_1", "acct_default"))).unwrap();
    assert!(mail.applied_actions().is_empty(), "paused applies nothing");
    let ntf = last_notification(&out, "classification_ready");
    let review = ntf["review_required_actions"].as_array().unwrap();
    assert_eq!(
        review.len(),
        1,
        "the action surfaces as a suggestion when paused"
    );
}

#[test]
fn accepting_a_retire_hot_reloads_so_the_rule_stops_firing_in_process() {
    // The Phase-7 adversarial-review HIGH fix: accepting a retire must hot-reload the engines in
    // place. Gating the reload on rule *creation* (not acceptance) left the just-retired rule firing
    // from the stale in-memory snapshot — it kept auto-applying the very action the user retired it
    // for, until a host restart. Seed an ACTIVE move rule into the initial snapshot, confirm it
    // auto-files, retire it, and confirm a later arrival is NOT auto-filed in the same process.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let rule_id = seed_active_rule(&backend, "file_stripe", "stripe.com", "Receipts");
    let router = router_over(&backend, out.clone());

    // Baseline: a stripe.com arrival is auto-filed to Receipts by the active rule.
    block_on(router.handle(new_mail_from("tb_1", "billing@stripe.com"))).unwrap();
    let before = last_notification(&out, "classification_ready");
    assert_eq!(
        before["applied_actions"][0]["kind"], "move",
        "active rule auto-files"
    );
    assert_eq!(before["applied_actions"][0]["to_folder"], "Receipts");

    // Surface a retire: three undos of this rule's action cross the decay count bar (each stamps an
    // `action_undone` row against the rule_id).
    for tb in ["tb_1", "tb_a", "tb_b"] {
        block_on(router.handle(request(
            "record_user_action",
            json!({
                "event_type": "action_undone",
                "action_kind": "move",
                "thunderbird_message_id": tb,
                "from_folder_id": "Receipts",
                "to_folder_id": "inbox",
                "rule_id": rule_id.as_str(),
                "user_initiated": true,
            }),
        )))
        .unwrap();
    }
    block_on(router.generate_proposals()).unwrap();
    let proposal_id = proposal_ready_notifications(&out)
        .into_iter()
        .find(|p| p["proposal_type"] == "retire_rule")
        .expect("a retire proposal surfaced for the high-undo rule")["proposal_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Accept the retire — this must hot-reload the engines (the fix).
    block_on(router.handle(request(
        "review_rule_proposal",
        json!({ "proposal_id": proposal_id, "decision": "accept_for_shadow_mode" }),
    )))
    .unwrap();
    assert_eq!(last_ok(&out)["resulting_status"], "accepted");

    // A subsequent stripe.com arrival is NO LONGER auto-filed — the retired rule stopped firing IN
    // PROCESS (before the fix it kept moving mail until the next restart).
    block_on(router.handle(new_mail_from("tb_2", "billing@stripe.com"))).unwrap();
    let after = last_notification(&out, "classification_ready");
    let applied = after["applied_actions"].as_array().map_or(0, Vec::len);
    assert_eq!(
        applied, 0,
        "the retired rule no longer auto-applies after the in-place reload: {after}"
    );
}

#[test]
fn auto_applied_fires_are_rule_stamped_so_the_undo_rate_drives_retirement_end_to_end() {
    // The keystone, proven through the whole production loop over the real host protocol:
    //   1. an active rule auto-files a stripe.com arrival → the applied move now carries `rule_id`
    //      on the wire (and an `action_applied` row stamped with that rule — the FIRES denominator);
    //   2. the extension echoes that `rule_id` back on Undo (we read it off the wire and pass it,
    //      exactly as background.js does) → each undo stamps an `action_undone` on the same rule;
    //   3. with 4 undos over 10 real fires, the decay pass governs on the true 40% undo-RATE — not
    //      the raw count — and surfaces a retire whose card states the real numbers.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let rule_id = seed_active_rule(&backend, "file_stripe", "stripe.com", "Receipts");
    let router = router_over(&backend, out.clone());

    // 10 stripe.com arrivals, each auto-filed by the active rule. The first proves the wire leg:
    // the applied move carries the authoring rule_id (the extension needs it to echo on Undo).
    for i in 0..10 {
        block_on(router.handle(new_mail_from(&format!("tb_f{i}"), "billing@stripe.com"))).unwrap();
        let applied = &last_notification(&out, "classification_ready")["applied_actions"][0];
        assert_eq!(applied["kind"], "move", "arrival {i} auto-filed");
        assert_eq!(
            applied["rule_id"].as_str(),
            Some(rule_id.as_str()),
            "the auto-applied fire carries its authoring rule_id on the wire (arrival {i})"
        );
    }

    // Undo 4 of the 10 — echoing the rule_id off the wire exactly as the extension does. 4/10 = 40%.
    for i in 0..4 {
        block_on(router.handle(request(
            "record_user_action",
            json!({
                "event_type": "action_undone",
                "action_kind": "move",
                "thunderbird_message_id": format!("tb_f{i}"),
                "from_folder_id": "Receipts",
                "to_folder_id": "inbox",
                "rule_id": rule_id.as_str(),
                "user_initiated": true,
            }),
        )))
        .unwrap();
    }

    block_on(router.generate_proposals()).unwrap();
    let retire = proposal_ready_notifications(&out)
        .into_iter()
        .find(|p| p["proposal_type"] == "retire_rule")
        .expect("a 40%-undo-rate rule over 10 real fires crosses the rate floor");
    let rationale = retire["rationale"].as_str().unwrap_or_default();
    assert!(
        rationale.contains("fired 10 times") && rationale.contains("40% undo rate"),
        "the retire card states the real undo-rate computed from stamped fires, not a bare count: {rationale}"
    );
}

#[test]
fn the_golden_path_correction_to_active_rule_to_autoapply_to_undo() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    // 1. CAPTURE — three consistent manual filings of stripe.com mail into Receipts. Each routes
    //    to the single-owner filing-feedback table.
    for (i, tb) in ["tb_1", "tb_2", "tb_3"].into_iter().enumerate() {
        block_on(router.handle(move_action(tb, "stripe.com", "Receipts"))).unwrap();
        assert_eq!(last_ok(&out)["sink"], "filing_feedback", "capture {i}");
    }

    // 2. MINE + BACK-TEST — the model-free proposal pass clusters the three moves, back-tests the
    //    candidate against the domain's whole history (clean → clears the 0.9 bar, G1), and
    //    surfaces ONE proposal for human review (recommended shadow, never auto-activated).
    block_on(router.generate_proposals()).unwrap();
    let ready = proposal_ready_notifications(&out);
    assert_eq!(ready.len(), 1, "exactly one back-tested proposal");
    assert_eq!(ready[0]["title"], "File stripe.com mail to Receipts");
    let proposal_id = ready[0]["proposal_id"].as_str().unwrap().to_owned();

    // 3. APPROVE & ACTIVATE — the explicit human "accept_active" turns the rule live. The response
    //    reports the rule MODE (active), distinct from the proposal disposition (G2).
    block_on(router.handle(request(
        "review_rule_proposal",
        json!({ "proposal_id": proposal_id, "decision": "accept_active" }),
    )))
    .unwrap();
    let reviewed = last_ok(&out);
    assert_eq!(reviewed["reviewed"], true);
    assert_eq!(
        reviewed["resulting_status"], "accepted",
        "the proposal disposition"
    );
    assert_eq!(reviewed["rule_status"], "active", "the rule went live (G2)");
    let rule_id = reviewed["rule_id"].as_str().unwrap().to_owned();
    assert!(rule_id.starts_with("rule_"));

    // 4. AUTO-APPLY (same process, no restart) — a fresh stripe.com arrival is auto-filed to
    //    Receipts BECAUSE the activation hot-reloaded the engines in place (G3). The applied move
    //    carries active-rule provenance and a reverses_to inverse for a real Undo.
    block_on(router.handle(new_mail_from("tb_new", "billing@stripe.com"))).unwrap();
    let classified = last_notification(&out, "classification_ready");
    let applied = &classified["applied_actions"][0];
    assert_eq!(applied["kind"], "move", "the activated rule auto-filed it");
    assert_eq!(applied["to_folder"], "Receipts");
    assert_eq!(applied["apply_state"], "auto_applied");
    assert_eq!(applied["authored_by"], "active_rule");
    assert_eq!(applied["reverses_to"]["kind"], "move");
    assert_eq!(applied["reverses_to"]["to_folder"], "inbox");

    // 5. UNDO — the user reverses the auto-applied move. It lands as NEGATIVE filing evidence
    //    against the rule's target, closing the loop honestly (a high-undo rule would later be
    //    surfaced for retirement).
    block_on(router.handle(request(
        "record_user_action",
        json!({
            "event_type": "action_undone",
            "action_kind": "move",
            "thunderbird_message_id": "tb_new",
            "from_folder_id": "Receipts",
            "to_folder_id": "inbox",
            "rule_id": rule_id,
            "user_initiated": true,
        }),
    )))
    .unwrap();
    assert_eq!(
        last_ok(&out)["sink"],
        "filing_feedback",
        "the Undo is captured"
    );

    // The undo row is negative evidence naming the rule's now-reversed target.
    let filing = SqliteFeedbackRepository::new(backend.clone());
    let rows = block_on(FeedbackRepository::<FilingFeedback>::query(
        &filing,
        FilingFeedbackQuery::default(),
    ))
    .unwrap();
    let undo_row = rows
        .iter()
        .find(|r| r.ai_suggested_folder == Some(FolderId::from("Receipts")))
        .expect("an undo row diverging from the Receipts target");
    assert_eq!(undo_row.human_chosen_folder, FolderId::from("inbox"));
    assert_eq!(undo_row.polarity, FeedbackPolarity::Negative);
}

/// A `triage_existing_mail` message entry sitting in `folder`, from `from`.
fn backfill_item(tb: &str, from: &str, folder: &str) -> Value {
    json!({
        "thunderbird_message_id": tb,
        "account_id": "acct_default",
        "folder_id": folder,
        "headers": { "from": from, "subject": "old mail" },
        "body_retention_allowed": false,
    })
}

#[test]
fn triage_existing_mail_classifies_without_apply_and_mines_folder_placements() {
    // The first-run backfill (Phase 2 exit): a sweep of existing mail classifies WITHOUT mutating
    // anything, mines deliberate folder placements as implicit positives, and — through the
    // ordinary proposal pass — populates the Review queue. The inbox is never mined.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    // Three stripe.com mails the user long ago filed into Receipts, plus one un-triaged inbox mail.
    block_on(router.handle(request(
        "triage_existing_mail",
        json!({
            "messages": [
                backfill_item("o1", "billing@stripe.com", "Receipts"),
                backfill_item("o2", "receipts@stripe.com", "Receipts"),
                backfill_item("o3", "no-reply@stripe.com", "Receipts"),
                backfill_item("o4", "someone@example.test", "inbox"),
            ],
        }),
    )))
    .unwrap();

    // One ok response — and crucially NO mail commands and NO classification_ready notifications:
    // the backfill mutates no mail.
    let frames = out.sent_frames();
    assert_eq!(
        frames.len(),
        1,
        "exactly one response, nothing applied: {frames:?}"
    );
    let summary = one_ok_response(&out);
    assert_eq!(summary["classified"], 4, "every message was classified");
    assert_eq!(
        summary["placements_recorded"], 3,
        "only the three deliberately-filed (non-inbox) messages are mined"
    );

    // The mined rows are implicit positives, basis-stamped to distinguish them from corrections.
    let filing = SqliteFeedbackRepository::new(backend.clone());
    let rows = block_on(FeedbackRepository::<FilingFeedback>::query(
        &filing,
        FilingFeedbackQuery::default(),
    ))
    .unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows
        .iter()
        .all(|r| r.basis.as_deref() == Some("existing_placement")
            && r.polarity == FeedbackPolarity::Positive
            && r.sender_domain.as_deref() == Some("stripe.com")));

    // Folder-history mining warmed the cluster: the ordinary proposal pass now surfaces a filing
    // proposal into the Review queue — backfill populated the queue without touching a single mail.
    block_on(router.generate_proposals()).unwrap();
    let ready = proposal_ready_notifications(&out);
    assert_eq!(ready.len(), 1, "the warmed cluster crossed the bar");
    assert_eq!(ready[0]["title"], "File stripe.com mail to Receipts");
}

#[test]
fn re_running_the_backfill_does_not_double_count_existing_placements() {
    // Idempotency: a re-tap of "triage existing mail" (or a host restart mid-run) must NOT re-mine
    // placements it already observed — duplicate implicit positives would inflate a cluster's
    // support for the SAME underlying messages. A message lives in one folder; mined once.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    let batch = json!({
        "messages": [
            backfill_item("o1", "billing@stripe.com", "Receipts"),
            backfill_item("o2", "receipts@stripe.com", "Receipts"),
        ],
    });

    block_on(router.handle(request("triage_existing_mail", batch.clone()))).unwrap();
    assert_eq!(
        one_ok_response(&out)["placements_recorded"],
        2,
        "first run mines both"
    );

    // A second identical sweep mines NOTHING new (the partial-unique index makes it idempotent).
    block_on(router.handle(request("triage_existing_mail", batch))).unwrap();
    assert_eq!(
        out.sent_frames().last().and_then(|f| match f {
            Frame::Response {
                payload: Some(p), ..
            } => Some(p["placements_recorded"].clone()),
            _ => None,
        }),
        Some(json!(0)),
        "the re-run double-counts nothing"
    );

    // The store holds exactly the two original placements — no duplicates piled up.
    let filing = SqliteFeedbackRepository::new(backend.clone());
    let rows = block_on(FeedbackRepository::<FilingFeedback>::query(
        &filing,
        FilingFeedbackQuery::default(),
    ))
    .unwrap();
    assert_eq!(
        rows.len(),
        2,
        "exactly the two observed placements, no duplicates"
    );
}

#[test]
fn bootstrap_seeds_the_starter_rules_as_drafts_idempotently() {
    // Cold-start (§3.5): a fresh install seeds two disabled starter rules to review/one-tap, and
    // re-running the bootstrap on a later launch adds nothing (a name collision is skipped).
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    let first = block_on(router.bootstrap_starter_rules()).unwrap();
    assert_eq!(
        first.imported, 2,
        "both starter rules are seeded on first run"
    );
    assert!(first.skipped.is_empty());

    // They are non-firing DRAFTS — neither the active nor the shadow snapshot of either kind
    // returns a starter rule, so a fresh install auto-applies nothing until the user activates one.
    use mailmate_common::rules::rule::{RuleKind, RuleScope};
    let rules = mailmate_storage::SqliteRuleRepository::new(backend.clone());
    for kind in [RuleKind::Classification, RuleKind::Action] {
        assert!(block_on(rules.get_active_rules(kind, RuleScope::Global))
            .unwrap()
            .is_empty());
        assert!(block_on(rules.get_shadow_rules(kind, RuleScope::Global))
            .unwrap()
            .is_empty());
    }

    // Idempotent: a second bootstrap (a later host launch) seeds nothing new.
    let second = block_on(router.bootstrap_starter_rules()).unwrap();
    assert_eq!(second.imported, 0, "re-running adds no duplicates");
    assert_eq!(second.skipped.len(), 2, "both already present");
}

#[test]
fn intake_stores_a_body_and_lowering_retention_purges_it_through_the_live_host() {
    // The carried Phase-1 deferral + the privacy contract: a background arrival is persisted to
    // the message store with its body ONLY while retention allows it, and the moment the user
    // turns the dial down, the already-stored bodies are erased ("turn it down and they're gone").
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());
    let messages = SqliteMessageRepository::new(backend.clone());

    // 1. Opt into body retention (level + affirmative consent → effective `bodies`).
    block_on(router.handle(request(
        "set_settings",
        json!({ "retention_level": "bodies", "body_consent": true }),
    )))
    .unwrap();

    // 2. A background arrival carrying a body is stored WITH its readable body.
    block_on(router.handle(new_mail_from("tb_keep", "billing@stripe.com"))).unwrap();
    let stored = block_on(messages.list_by_status(ClassificationStatus::Pending)).unwrap();
    assert_eq!(stored.len(), 1, "the arrival was persisted");
    assert_eq!(
        stored[0].body_text.as_deref(),
        Some("thanks for your payment"),
        "the body is retained while consent stands"
    );

    // 3. Lower the dial to metadata — the stored body is purged at once; the row (identity) stays.
    block_on(router.handle(request(
        "set_settings",
        json!({ "retention_level": "metadata" }),
    )))
    .unwrap();
    let after = block_on(messages.list_by_status(ClassificationStatus::Pending)).unwrap();
    assert_eq!(after.len(), 1, "the message identity row is kept");
    assert!(
        after[0].body_text.is_none(),
        "the readable body is gone after the down-level purge"
    );
}

#[test]
fn the_back_test_gate_withholds_an_inconsistently_filed_domain_in_the_live_host() {
    // The negative control through the real composition root: a domain the user files
    // inconsistently (3 → Archive crosses the count bar, but 2 → Keep contradict it) fails the
    // back-test precision bar and is NEVER surfaced — proving the gate is wired into the live
    // proposal pass, not just unit-tested in isolation.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_over(&backend, out.clone());

    for tb in ["n1", "n2", "n3"] {
        block_on(router.handle(move_action(tb, "noisy.test", "Archive"))).unwrap();
    }
    for tb in ["n4", "n5"] {
        block_on(router.handle(move_action(tb, "noisy.test", "Keep"))).unwrap();
    }

    block_on(router.generate_proposals()).unwrap();
    assert!(
        proposal_ready_notifications(&out).is_empty(),
        "an inconsistently-filed domain is withheld by the back-test gate"
    );
    let proposals = SqliteProposalRepository::new(backend.clone());
    assert!(
        block_on(proposals.list_by_status(ProposalStatus::PendingReview))
            .unwrap()
            .is_empty(),
        "nothing persisted for the contradicted candidate"
    );
}
