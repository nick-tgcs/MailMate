//! Behavioural tests for the Phase-12 management surface of the [`HostRouter`]:
//! `explain_decision` (the audit timeline for a message), `list_pending_reviews` (the review
//! queue), and `get_settings` (the secret-free settings snapshot) — driven over a *real*
//! `build_router` composition against an in-memory database, with the stores seeded directly.

use std::sync::Arc;

use futures::executor::block_on;
use serde_json::{json, Value};

use mailmate_common::actor::Actor;
use mailmate_common::audit::AuditEntry;
use mailmate_common::feedback::{
    FeedbackPolarity, FilingFeedback, FilingFeedbackRow, PinnedVersions,
};
use mailmate_common::ids::{FolderId, MessageId, ProposalId};
use mailmate_common::proposal::{AgentProposal, ProposalKind, ProposalStatus};
use mailmate_common::protocol::{Frame, ProtocolVersion, ResponseStatus};
use mailmate_common::rules::rule::{RiskLevel, RuleStatus};
use mailmate_common::time::Timestamp;
use mailmate_native_host::config::AppConfig;
use mailmate_native_host::router::HostRouter;
use mailmate_native_host::runtime::build_router;
use mailmate_ports::storage::{AuditRepository, FeedbackRepository, ProposalRepository};
use mailmate_storage::{
    open_and_migrate, SqliteAuditRepository, SqliteBackend, SqliteFeedbackRepository,
    SqliteProposalRepository, StorageConfig,
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
    build_router(&AppConfig::default(), backend, out, secrets).unwrap()
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
    let router = build_router(&config, &backend, out.clone(), secrets).unwrap();

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

    block_on(router.handle(request(
        "set_settings",
        json!({ "retention_level": "summaries", "follow_up_tick_seconds": 600 }),
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
