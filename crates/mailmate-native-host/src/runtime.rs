//! The production composition root: assemble the concrete adapters into a live
//! [`HostRouter`] and serve it.
//!
//! This is the wiring the earlier phases deferred. [`build_router`] constructs the full
//! [`Ports`] graph — the SQLite repositories, the deterministic rule engines (seeded from the
//! stored rule snapshots), the cascade classifier, the action planner, the hard policy guard,
//! the learning loop, the curator, the proposal-review step, the reply drafter, and the
//! training pipeline — plus the follow-up [`FollowUpSuite`], all behind the ports the core
//! already speaks. The leaf adapters ([`SystemClock`], [`FileSecretStore`],
//! [`DeterministicFeatureExtractor`]) are injected here and nowhere else.
//!
//! **Zero-provider by design.** The provider registry ships empty; every provider-typed
//! collaborator (the reply drafter, the curator, the training evaluator) is wired to
//! [`UnavailableProvider`], so an LLM-always task *degrades* (a Tier-3-needed message → review;
//! a draft request → an error the host surfaces) instead of fabricating output. MailMate is
//! fully functional like this: Tier 1/2 classification, the rule/policy/audit spine, the
//! learning loop, follow-up scheduling, and review-required draft slots all run with no LLM.
//! Wiring a live network provider additionally needs the edge HTTP client (the one remaining
//! edge wire, mirrored on every other adapter); the `[ai]` config is parsed, validated, and
//! surfaced today.
//!
//! [`serve`] mounts the router on the stdio transport, runs the catch-up-on-launch drain, and
//! (when `tick_seconds > 0`) spawns the in-host follow-up worker before entering the loop.

use std::io::{stdin, stdout};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use futures::executor::block_on;

use mailmate_common::error::{StorageError, TransportError};
use mailmate_common::feedback::{
    ClassificationFeedback, FilingFeedback, FollowUpFeedback, RuleProposalFeedback,
};
use mailmate_common::rules::rule::{EvaluatableRule, RuleKind, RuleScope};
use mailmate_core::{ImportExportService, Ports};
use mailmate_ports::ai_provider::AiProvider;
use mailmate_ports::clock::Clock;
use mailmate_ports::feature_extractor::FeatureExtractor;
use mailmate_ports::rule_engine::RuleEngine;
use mailmate_ports::storage::{FeedbackRepository, RuleRepository};
use mailmate_ports::transport::Transport;

use mailmate_ai::http::HttpClient;
use mailmate_ai::TaskReplyDrafter;
use mailmate_learning::{AiRuleCurator, DefaultLearningEngine, DefaultProposalReview};
use mailmate_ml::{DeterministicFeatureExtractor, InProcessTrainer, LogisticRegressionClassifier};
use mailmate_planner::{CascadeClassifier, DefaultActionPlanner};
use mailmate_policy::HardPolicyGuard;
use mailmate_rules::DeterministicRuleEngine;
use mailmate_storage::{
    open_and_migrate, restore_database, SqliteAdapterRepository, SqliteAuditRepository,
    SqliteBackend, SqliteConflictRepository, SqliteDatasetRepository, SqliteEvalRunRepository,
    SqliteFeedbackRepository, SqlitePipelineItemRepository, SqliteProposalRepository,
    SqliteRuleRepository, SqliteWorkflowInstanceRepository, SqliteWorkflowRepository,
    StorageConfig, StoragePath,
};
use mailmate_training::{DefaultTrainingPipeline, FeedbackTrainingSource};
use mailmate_workflow::{DefaultExitDetector, DefaultFollowUpScheduler, DefaultWorkflowEngine};

use crate::clock::SystemClock;
use crate::config::{AppConfig, ConfigError};
use crate::http_client::StdHttpClient;
use crate::provider::build_provider;
use crate::router::{AdminSuite, FollowUpSuite, HostRouter};
use crate::secret_store::FileSecretStore;
use crate::thunderbird::{ThunderbirdMailClient, WriterTransport};

/// Every scope a rule snapshot sweeps when seeding the deterministic engines.
const SCOPES: [RuleScope; 5] = [
    RuleScope::Global,
    RuleScope::Account,
    RuleScope::Folder,
    RuleScope::Sender,
    RuleScope::Domain,
];

/// The positive/negative labels the always-on Tier-2 logistic classifier discriminates.
const TIER2_POSITIVE: &str = "spam";
const TIER2_NEGATIVE: &str = "ham";

/// A failure standing up or running the host.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// A configuration failure.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A storage failure (open / migrate / repository).
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
    /// A transport failure that ended the serve loop.
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    /// A filesystem failure reading/writing a maintenance artifact.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A maintenance artifact could not be (de)serialized.
    #[error("serialization error: {0}")]
    Serialization(String),
}

/// A fully-wired host: the live router, the storage backend (for maintenance ops), and the
/// effective config.
pub struct App {
    /// The live router, ready to serve or drain.
    pub router: HostRouter,
    /// The storage backend (kept for backup and direct maintenance).
    pub backend: Arc<SqliteBackend>,
    /// The effective configuration.
    pub config: AppConfig,
}

/// Open + migrate the configured database and assemble a live [`App`] whose router emits
/// through `out`.
///
/// # Errors
/// [`RuntimeError::Storage`] if the database cannot be opened/migrated, or a repository read
/// (rule-snapshot seeding) fails.
pub fn build_app(
    config: AppConfig,
    data_dir: &Path,
    out: Arc<dyn Transport>,
) -> Result<App, RuntimeError> {
    let storage_config = config.storage_config(data_dir);
    let backend = open_and_migrate(&storage_config)?;
    let secrets = secrets_path(&storage_config, data_dir);
    let router = build_router(&config, &backend, out, secrets)?;
    Ok(App {
        router,
        backend,
        config,
    })
}

/// Assemble the full [`Ports`] graph + follow-up suite over `backend`, returning a router
/// that emits through `out`. Pure wiring (no I/O beyond seeding the rule snapshots), so it is
/// driven both by [`build_app`] in production and by the composition tests with an in-memory
/// backend + a fake transport.
///
/// # Errors
/// [`RuntimeError::Storage`] if seeding a rule snapshot from the repository fails.
pub fn build_router(
    config: &AppConfig,
    backend: &Arc<SqliteBackend>,
    out: Arc<dyn Transport>,
    secrets_path: PathBuf,
) -> Result<HostRouter, RuntimeError> {
    // --- Repositories (all over the one backend) ---
    let rules = Arc::new(SqliteRuleRepository::new(backend.clone()));
    let audit = Arc::new(SqliteAuditRepository::new(backend.clone()));
    let feedback = Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    let proposals = Arc::new(SqliteProposalRepository::new(backend.clone()));
    let conflicts = Arc::new(SqliteConflictRepository::new(backend.clone()));
    let items = Arc::new(SqlitePipelineItemRepository::new(backend.clone()));
    let workflows = Arc::new(SqliteWorkflowRepository::new(backend.clone()));
    let instances = Arc::new(SqliteWorkflowInstanceRepository::new(backend.clone()));
    let datasets = Arc::new(SqliteDatasetRepository::new(backend.clone()));
    let lora_adapters = Arc::new(SqliteAdapterRepository::new(backend.clone()));
    let eval_runs = Arc::new(SqliteEvalRunRepository::new(backend.clone()));

    // The one feedback store, viewed as each single-owner kind.
    let classification_feedback: Arc<dyn FeedbackRepository<ClassificationFeedback>> =
        feedback.clone();
    let filing_feedback: Arc<dyn FeedbackRepository<FilingFeedback>> = feedback.clone();
    let rule_proposal_feedback: Arc<dyn FeedbackRepository<RuleProposalFeedback>> =
        feedback.clone();
    let followup_feedback: Arc<dyn FeedbackRepository<FollowUpFeedback>> = feedback.clone();

    // --- Leaf adapters ---
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
    let secret_store = Arc::new(FileSecretStore::new(secrets_path));
    let feature_extractor: Arc<dyn FeatureExtractor> =
        Arc::new(DeterministicFeatureExtractor::new());
    let tier2 = Arc::new(LogisticRegressionClassifier::new(
        TIER2_POSITIVE,
        TIER2_NEGATIVE,
    ));
    // Build the configured provider from `[ai]` over the host's local HTTP transport. With no
    // provider configured (or an incomplete one), this degrades to UnavailableProvider — every
    // provider-typed collaborator still stands up and honestly reports "needs a provider".
    let http_client: Arc<dyn HttpClient> = Arc::new(StdHttpClient::new());
    let provider: Arc<dyn AiProvider> =
        build_provider(&config.ai, secret_store.as_ref(), http_client.clone());

    // --- Deterministic engines, seeded from the stored rule snapshots ---
    let classification_snapshot = rule_snapshot(rules.as_ref(), RuleKind::Classification)?;
    let action_snapshot = rule_snapshot(rules.as_ref(), RuleKind::Action)?;
    let mut all_rules = classification_snapshot.clone();
    all_rules.extend(action_snapshot.clone());

    let classification_rule_engine: Arc<dyn RuleEngine> =
        Arc::new(DeterministicRuleEngine::new(classification_snapshot));
    let action_rule_engine: Arc<dyn RuleEngine> =
        Arc::new(DeterministicRuleEngine::new(action_snapshot));
    let curator_rule_engine: Arc<dyn RuleEngine> =
        Arc::new(DeterministicRuleEngine::new(all_rules));

    let classification_engine = Arc::new(CascadeClassifier::new(
        classification_rule_engine,
        tier2.clone(),
    ));
    let action_planner = Arc::new(DefaultActionPlanner::new(action_rule_engine));
    let policy_guard = Arc::new(HardPolicyGuard::new());

    // --- The learning loop + curator + review + drafter + training ---
    let learning_engine = Arc::new(DefaultLearningEngine::new(
        classification_feedback.clone(),
        filing_feedback.clone(),
        audit.clone(),
        proposals.clone(),
    ));
    let rule_curator = Arc::new(AiRuleCurator::new(
        provider.clone(),
        learning_engine.clone(),
        rules.clone(),
        curator_rule_engine,
        proposals.clone(),
        conflicts.clone(),
        audit.clone(),
    ));
    let proposal_review = Arc::new(DefaultProposalReview::new(
        proposals.clone(),
        rules.clone(),
        workflows.clone(),
        rule_proposal_feedback.clone(),
        audit.clone(),
    ));
    let reply_drafter = Arc::new(TaskReplyDrafter::new(provider.clone()));
    let training_source = Arc::new(FeedbackTrainingSource::new(
        classification_feedback,
        filing_feedback,
        rule_proposal_feedback,
    ));
    let training_pipeline = Arc::new(DefaultTrainingPipeline::new(
        training_source,
        Arc::new(InProcessTrainer::new()),
        datasets,
        lora_adapters,
        eval_runs,
        provider,
        clock.clone(),
        config.retention_level(),
    ));

    // --- The mail client over the shared single-writer transport ---
    let mail_client = Arc::new(ThunderbirdMailClient::new(out.clone()));

    let ports = Ports {
        mail_client,
        transport: out.clone(),
        clock: clock.clone(),
        secret_store: secret_store.clone(),
        feature_extractor,
        tier2,
        classification_engine,
        action_planner,
        policy_guard,
        learning_engine,
        rule_curator,
        proposal_review,
        reply_drafter: reply_drafter.clone(),
        training_pipeline,
    };

    // --- The follow-up suite (lives outside the core's Ports; the core never schedules) ---
    let suite = FollowUpSuite {
        pipeline_items: items.clone(),
        instances: instances.clone(),
        workflow_engine: Arc::new(DefaultWorkflowEngine::new(
            workflows.clone(),
            instances.clone(),
            items.clone(),
            clock,
        )),
        scheduler: Arc::new(DefaultFollowUpScheduler::new(
            workflows,
            instances.clone(),
            items.clone(),
            followup_feedback,
            reply_drafter,
            audit.clone(),
        )),
        exit_detector: Arc::new(DefaultExitDetector::new(instances, items)),
    };

    let admin = AdminSuite {
        proposals,
        // The live, mutable config the `set_*` writes mutate; seeded from the loaded config.
        config: Arc::new(Mutex::new(config.clone())),
        // Persist writes back to the file the config came from (`MAILMATE_CONFIG`), if any.
        config_path: env_nonempty("MAILMATE_CONFIG").map(PathBuf::from),
        secret_store,
        // The shared HTTP transport, reused for `list_models` provider discovery.
        http: http_client,
    };

    Ok(HostRouter::from_ports(&ports, audit, out)
        .with_followups(suite)
        .with_admin(admin))
}

/// Run the host: open the database, assemble the router over the stdio transport, drain
/// overdue follow-ups (catch-up-on-launch), optionally spawn the periodic worker, then serve
/// the loop until the peer hangs up.
///
/// # Errors
/// [`RuntimeError`] if the app cannot be built, the startup drain fails to emit, or the serve
/// loop ends on a fatal transport error.
pub fn serve(config: AppConfig, data_dir: &Path) -> Result<(), RuntimeError> {
    let out: Arc<dyn Transport> = Arc::new(WriterTransport::new(stdout()));
    let app = build_app(config, data_dir, out)?;

    if app.config.followups.catch_up_on_launch {
        block_on(app.router.drain_followups())?;
    }

    // Mine recurring feedback for deterministic rule candidates at launch and surface any new
    // proposals into the review queue. Model-free (runs with zero providers) and idempotent
    // (re-running adds nothing) — the periodic ticker repeats it alongside the follow-up drain.
    block_on(app.router.generate_proposals())?;

    let stop = Arc::new(AtomicBool::new(false));
    let ticker = (app.config.followups.tick_seconds > 0).then(|| {
        spawn_ticker(
            app.router.clone(),
            Duration::from_secs(app.config.followups.tick_seconds),
            stop.clone(),
        )
    });

    let mut reader = stdin().lock();
    let result = app.router.serve_blocking(&mut reader);

    // Tear the worker down before returning so the process exits cleanly on EOF.
    stop.store(true, Ordering::Relaxed);
    if let Some(handle) = ticker {
        let _ = handle.join();
    }
    result.map_err(RuntimeError::from)
}

/// Back up the configured database to `dest` (a consistent `VACUUM INTO` snapshot).
///
/// # Errors
/// [`RuntimeError::Storage`] if the database cannot be opened or the snapshot fails.
pub fn backup(config: &AppConfig, data_dir: &Path, dest: &Path) -> Result<(), RuntimeError> {
    let backend = open_and_migrate(&config.storage_config(data_dir))?;
    backend.backup_to(dest)?;
    Ok(())
}

/// Restore the configured database from `src` (offline; replaces the live file).
///
/// # Errors
/// [`RuntimeError::Storage`] if the snapshot is invalid or the swap fails.
pub fn restore(config: &AppConfig, data_dir: &Path, src: &Path) -> Result<(), RuntimeError> {
    restore_database(&config.storage_config(data_dir), src)?;
    Ok(())
}

/// Export the operative rule set to `dest` as a JSON [`RuleManifest`](mailmate_common::rules::manifest::RuleManifest).
/// Returns the number of rules written.
///
/// # Errors
/// [`RuntimeError`] on a storage or I/O/serialization failure.
pub fn export_rules(
    config: &AppConfig,
    data_dir: &Path,
    dest: &Path,
) -> Result<usize, RuntimeError> {
    let backend = open_and_migrate(&config.storage_config(data_dir))?;
    let service = ImportExportService::new(Arc::new(SqliteRuleRepository::new(backend)));
    let manifest = block_on(service.export())?;
    let json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| RuntimeError::Serialization(e.to_string()))?;
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(dest, json)?;
    Ok(manifest.len())
}

/// Import rules from a JSON manifest at `src`, naming each `<prefix>-<source-id>` and landing
/// it as a draft. Returns the count imported and the count skipped.
///
/// # Errors
/// [`RuntimeError`] on a storage, I/O, or deserialization failure.
pub fn import_rules(
    config: &AppConfig,
    data_dir: &Path,
    src: &Path,
    name_prefix: &str,
) -> Result<(usize, usize), RuntimeError> {
    let text = std::fs::read_to_string(src)?;
    let manifest =
        serde_json::from_str(&text).map_err(|e| RuntimeError::Serialization(e.to_string()))?;
    let backend = open_and_migrate(&config.storage_config(data_dir))?;
    let service = ImportExportService::new(Arc::new(SqliteRuleRepository::new(backend)));
    let summary = block_on(service.import(&manifest, name_prefix))?;
    Ok((summary.imported, summary.skipped.len()))
}

/// Resolve the platform data directory: `MAILMATE_DATA_DIR`, else `$XDG_DATA_HOME/mailmate`,
/// else `$HOME/.local/share/mailmate`, else a temp-dir fallback.
#[must_use]
pub fn resolve_data_dir() -> PathBuf {
    if let Some(dir) = env_nonempty("MAILMATE_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = env_nonempty("XDG_DATA_HOME") {
        return PathBuf::from(dir).join("mailmate");
    }
    if let Some(home) = env_nonempty("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("mailmate");
    }
    std::env::temp_dir().join("mailmate")
}

/// Resolve the effective config: load `MAILMATE_CONFIG` if set, else the default config.
///
/// # Errors
/// [`RuntimeError::Config`] if `MAILMATE_CONFIG` points at a file that fails to load/validate.
pub fn resolve_config() -> Result<AppConfig, RuntimeError> {
    match env_nonempty("MAILMATE_CONFIG") {
        Some(path) => Ok(AppConfig::load(Path::new(&path))?),
        None => Ok(AppConfig::default()),
    }
}

/// The secret file lives beside the database (or under the data dir for an in-memory DB).
fn secrets_path(storage: &StorageConfig, data_dir: &Path) -> PathBuf {
    match &storage.path {
        StoragePath::File(db) => db.parent().map_or_else(
            || data_dir.join("secrets.json"),
            |dir| dir.join("secrets.json"),
        ),
        StoragePath::InMemory => data_dir.join("secrets.json"),
    }
}

/// The active+shadow rule snapshot of `kind` across every scope — the immutable view the
/// deterministic engine evaluates. Empty on a fresh database (the zero-config posture).
fn rule_snapshot(
    rules: &dyn RuleRepository,
    kind: RuleKind,
) -> Result<Vec<EvaluatableRule>, StorageError> {
    let mut snapshot = Vec::new();
    for scope in SCOPES {
        snapshot.extend(block_on(rules.get_active_rules(kind, scope))?);
        snapshot.extend(block_on(rules.get_shadow_rules(kind, scope))?);
    }
    Ok(snapshot)
}

/// A non-empty environment variable, trimmed-empty treated as unset.
fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Spawn the in-host follow-up worker: drain due steps every `tick`, until `stop` is set.
fn spawn_ticker(router: HostRouter, tick: Duration, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    thread::spawn(move || run_ticker(&router, tick, &stop))
}

/// The worker body: sleep in small steps (so shutdown stays responsive), then drain. A drain
/// failure is swallowed inside `drain_followups` (it audits and continues), so the worker
/// never tears down the host on a transient storage hiccup.
fn run_ticker(router: &HostRouter, tick: Duration, stop: &AtomicBool) {
    let step = Duration::from_millis(100);
    while !stop.load(Ordering::Relaxed) {
        let mut slept = Duration::ZERO;
        while slept < tick && !stop.load(Ordering::Relaxed) {
            let nap = step.min(tick - slept);
            thread::sleep(nap);
            slept += nap;
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let _ = block_on(router.drain_followups());
        let _ = block_on(router.generate_proposals());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use mailmate_test_support::fakes::FakeTransport;

    fn data_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn build_app_stands_up_a_router_over_an_in_memory_db() {
        let mut config = AppConfig::default();
        config.storage.database_path = Some("memory".to_owned());
        let out = Arc::new(FakeTransport::new());
        let app = build_app(config, data_dir().path(), out).unwrap();
        // A fresh DB is migration-current.
        assert_eq!(
            app.backend.applied_migration_versions().unwrap(),
            vec![1, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn resolve_data_dir_prefers_the_explicit_env_override() {
        // Safe: this test owns the override and asserts only the override branch.
        std::env::set_var("MAILMATE_DATA_DIR", "/tmp/mailmate-test-dir");
        assert_eq!(resolve_data_dir(), PathBuf::from("/tmp/mailmate-test-dir"));
        std::env::remove_var("MAILMATE_DATA_DIR");
    }

    #[test]
    fn secrets_path_sits_beside_a_file_database() {
        let storage = StorageConfig::sqlite_file("/data/mailmate/db.sqlite");
        let path = secrets_path(&storage, Path::new("/fallback"));
        assert_eq!(path, PathBuf::from("/data/mailmate/secrets.json"));
        // In-memory falls back to the data dir.
        let mem = StorageConfig::sqlite_in_memory();
        assert_eq!(
            secrets_path(&mem, Path::new("/fallback")),
            PathBuf::from("/fallback/secrets.json")
        );
    }

    #[test]
    fn backup_then_restore_round_trips_a_file_database() {
        let dir = data_dir();
        let config = AppConfig {
            storage: crate::config::StorageSettings {
                database_path: Some(dir.path().join("live.db").display().to_string()),
            },
            ..AppConfig::default()
        };
        // Stand the DB up, back it up, wipe it, restore.
        let _ = open_and_migrate(&config.storage_config(dir.path())).unwrap();
        let snapshot = dir.path().join("snap.db");
        backup(&config, dir.path(), &snapshot).unwrap();
        std::fs::remove_file(dir.path().join("live.db")).unwrap();
        restore(&config, dir.path(), &snapshot).unwrap();
        // The restored DB opens and is current.
        let backend = open_and_migrate(&config.storage_config(dir.path())).unwrap();
        assert_eq!(
            backend.applied_migration_versions().unwrap(),
            vec![1, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn rule_export_import_round_trips_through_files() {
        let dir = data_dir();
        let config = AppConfig {
            storage: crate::config::StorageSettings {
                database_path: Some(dir.path().join("rules.db").display().to_string()),
            },
            ..AppConfig::default()
        };
        // Fresh DB: no rules → an empty manifest exports cleanly and imports nothing.
        let manifest_path = dir.path().join("rules.json");
        let count = export_rules(&config, dir.path(), &manifest_path).unwrap();
        assert_eq!(count, 0);
        let (imported, skipped) =
            import_rules(&config, dir.path(), &manifest_path, "copy").unwrap();
        assert_eq!((imported, skipped), (0, 0));
    }

    #[test]
    fn the_ticker_returns_immediately_when_already_stopped() {
        // Build a router with no follow-up wiring; a pre-stopped ticker does nothing.
        let mut config = AppConfig::default();
        config.storage.database_path = Some("memory".to_owned());
        let out = Arc::new(FakeTransport::new());
        let app = build_app(config, data_dir().path(), out).unwrap();
        let stop = AtomicBool::new(true);
        run_ticker(&app.router, Duration::from_secs(3600), &stop);
        // Returning at all (no 3600s sleep) is the assertion; nothing was emitted.
    }
}
