//! Phase-8 end-to-end probe: on-device Tier-2 training over the *real* `build_router`
//! composition. It seeds classification corrections, fires `train_tier2_model`, and proves the
//! whole vertical: read corrections → train a Burn model → eval the **reloaded** artifact on a
//! held-out split → gate on precision → promote a loadable artifact to disk → **hot-swap it
//! into the live cascade** so the very next classification is served by the trained model.
//!
//! The headline assertion is behavioural: a message from a domain the corrections marked spam
//! reads ~0.5 (undecided) BEFORE training and confidently spam AFTER — the cascade's verdict
//! moved because it is now using the freshly-trained, gated artifact, not the cold fallback.

use std::sync::Arc;

use futures::executor::block_on;
use serde_json::{json, Value};

use mailmate_common::features::{FeatureValue, FeatureVector};
use mailmate_common::feedback::{
    ClassificationFeedback, ClassificationFeedbackRow, FeedbackPolarity, PinnedVersions,
};
use mailmate_common::ids::{FeedbackId, MessageId};
use mailmate_common::protocol::{Frame, ProtocolVersion, ResponseStatus};
use mailmate_common::time::Timestamp;
use mailmate_native_host::config::AppConfig;
use mailmate_native_host::router::HostRouter;
use mailmate_native_host::runtime::build_router;
use mailmate_ports::storage::FeedbackRepository;
use mailmate_storage::{open_and_migrate, SqliteBackend, SqliteFeedbackRepository, StorageConfig};
use mailmate_test_support::fakes::FakeTransport;

fn request(type_: &str, payload: Value) -> Frame {
    Frame::Request {
        protocol_version: ProtocolVersion::default(),
        request_id: format!("req_{type_}"),
        type_: type_.to_owned(),
        payload,
    }
}

fn last_ok(out: &FakeTransport) -> Value {
    match out.sent_frames().last().expect("a response frame") {
        Frame::Response {
            status: ResponseStatus::Ok,
            payload: Some(payload),
            ..
        } => payload.clone(),
        other => panic!("expected an ok response, got {other:?}"),
    }
}

/// One classification correction: a single `sender_domain` feature (the controllable
/// discriminator) corrected to `label`.
fn correction(i: usize, domain: &str, label: &str) -> ClassificationFeedbackRow {
    let mut fv = FeatureVector::new();
    fv.insert("sender_domain", FeatureValue::Text(domain.to_owned()));
    ClassificationFeedbackRow {
        id: FeedbackId::from(format!("clsfb_{i}")),
        message_id: MessageId::from(format!("msg_{i}")),
        pinned_versions: PinnedVersions::default(),
        ai_label: None,
        ai_score: None,
        ai_rationale: None,
        human_label: label.to_owned(),
        human_reason_code: None,
        human_reason_text: None,
        salient_features: fv,
        polarity: FeedbackPolarity::Positive,
        created_at: Timestamp::now(),
    }
}

fn classify_from(domain: &str) -> Value {
    json!({
        "thunderbird_message_id": "probe1",
        "account_id": "acct_default",
        "folder_id": "inbox",
        "headers": { "from": format!("sender@{domain}"), "subject": "hello there" },
        "body_text": "hi",
        "body_retention_allowed": false
    })
}

fn spam_score(out: &FakeTransport) -> f64 {
    last_ok(out)["classification"]["spam_score"]
        .as_f64()
        .expect("a spam_score")
}

fn unique_tier2_path(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mm_tier2_e2e_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("tier2_weights.json")
}

fn router_with_tier2(
    backend: &Arc<SqliteBackend>,
    out: Arc<FakeTransport>,
    weights: std::path::PathBuf,
) -> HostRouter {
    let secrets = weights.with_file_name("secrets.json");
    build_router(
        &AppConfig::default(),
        backend,
        out,
        secrets,
        None,
        Some(weights),
    )
    .unwrap()
}

#[test]
fn training_from_corrections_activates_a_gated_artifact_and_the_cascade_serves_it() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    // Seed a separable correction corpus: spammer.test → spam, friend.test → newsletter.
    let feedback: Arc<dyn FeedbackRepository<ClassificationFeedback>> =
        Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    for i in 0..12 {
        block_on(feedback.append(correction(i, "spammer.test", "spam"))).unwrap();
    }
    for i in 12..24 {
        block_on(feedback.append(correction(i, "friend.test", "newsletter"))).unwrap();
    }

    let weights = unique_tier2_path("serve");
    let model_active = weights.with_file_name("tier2_model").join("active");
    let out = Arc::new(FakeTransport::new());
    let router = router_with_tier2(&backend, out.clone(), weights);

    // BEFORE: the cold fallback is undecided about a spammer.test message (~0.5).
    block_on(router.handle(request("classify_message", classify_from("spammer.test")))).unwrap();
    let cold_spam = spam_score(&out);
    assert!(
        (cold_spam - 0.5).abs() < 0.2,
        "cold model should be undecided, got {cold_spam}"
    );

    // TRAIN: read corrections → train → eval the reloaded artifact → gate → promote → hot-swap.
    block_on(router.handle(request("train_tier2_model", json!({})))).unwrap();
    let report = last_ok(&out);
    assert_eq!(
        report["activated"],
        json!(true),
        "should activate: {report}"
    );
    assert!(
        report["precision"].as_f64().unwrap() >= 0.8,
        "held-out precision must clear the gate: {report}"
    );
    assert!(report["eval_n"].as_u64().unwrap() >= 4, "{report}");
    assert!(report["train_count"].as_u64().unwrap() > 0, "{report}");
    assert_eq!(report["calibration_version"], json!("burn-linear-v1"));
    assert!(
        model_active.join("model.mpk").exists(),
        "the loadable Burn artifact must be on disk at {model_active:?}"
    );
    assert!(model_active.join("manifest.json").exists());

    // AFTER: the SAME message is now confidently spam — the cascade is serving the swapped,
    // freshly-trained Burn artifact (the verdict moved with no other change).
    block_on(router.handle(request("classify_message", classify_from("spammer.test")))).unwrap();
    let warm_spam = spam_score(&out);
    assert!(warm_spam > 0.5, "trained model says spam, got {warm_spam}");
    assert!(
        warm_spam > cold_spam + 0.2,
        "the cascade's verdict moved after training: {cold_spam} -> {warm_spam}"
    );

    let _ = std::fs::remove_dir_all(model_active.parent().unwrap().parent().unwrap());
}

#[test]
fn training_with_no_corrections_reports_not_activated_and_never_fabricates_a_model() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let weights = unique_tier2_path("empty");
    let model_active = weights.with_file_name("tier2_model").join("active");
    let out = Arc::new(FakeTransport::new());
    let router = router_with_tier2(&backend, out.clone(), weights);

    block_on(router.handle(request("train_tier2_model", json!({})))).unwrap();
    let report = last_ok(&out);
    assert_eq!(
        report["activated"],
        json!(false),
        "no data ⇒ no activation: {report}"
    );
    assert_eq!(report["eval_n"], json!(0));
    assert!(
        !model_active.exists(),
        "nothing was promoted to active from an empty corpus"
    );

    let _ = std::fs::remove_dir_all(model_active.parent().unwrap().parent().unwrap());
}
