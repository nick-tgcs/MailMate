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
use mailmate_common::rules::rule::{EvaluatableRule, RuleKind, RuleScope, RuleStatus};
use mailmate_core::{ImportExportService, Ports};
use mailmate_ports::ai_provider::AiProvider;
use mailmate_ports::clock::Clock;
use mailmate_ports::feature_extractor::FeatureExtractor;
use mailmate_ports::rule_engine::RuleEngine;
use mailmate_ports::storage::{FeedbackRepository, RuleRepository};
use mailmate_ports::tier2_classifier::Tier2Classifier;
use mailmate_ports::transport::Transport;

use mailmate_ai::http::HttpClient;
use mailmate_ai::{SwappableProvider, TaskReplyDrafter};
use mailmate_learning::{AiRuleCurator, DefaultLearningEngine, DefaultProposalReview};
use mailmate_ml::{
    DeterministicFeatureExtractor, InProcessTrainer, LogisticRegressionClassifier, SwappableTier2,
};
use mailmate_planner::{CascadeClassifier, DefaultActionPlanner};
use mailmate_policy::HardPolicyGuard;
use mailmate_rules::DeterministicRuleEngine;
use mailmate_storage::{
    ensure_dir_owner_only, open_and_migrate, restore_database, SqliteAdapterRepository,
    SqliteAuditRepository, SqliteBackend, SqliteConflictRepository, SqliteDataRightsRepository,
    SqliteDatasetRepository, SqliteEvalRunRepository, SqliteFeedbackRepository,
    SqliteMessageRepository, SqlitePipelineItemRepository, SqliteProposalRepository,
    SqliteReminderRepository, SqliteRuleRepository, SqliteWorkflowInstanceRepository,
    SqliteWorkflowRepository, StorageConfig, StoragePath,
};
use mailmate_training::{DefaultTrainingPipeline, FeedbackTrainingSource};
use mailmate_workflow::{DefaultExitDetector, DefaultFollowUpScheduler, DefaultWorkflowEngine};

use crate::clock::SystemClock;
use crate::config::{AppConfig, ConfigError};
use crate::http_client::StdHttpClient;
use crate::provider::build_provider;
use crate::router::{AdminSuite, FollowUpSuite, HostRouter, RuleReload};
use crate::secret_store::FileSecretStore;
use crate::thunderbird::{ThunderbirdMailClient, WriterTransport};
use crate::tier2_training::Tier2TrainingService;

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
    let tier2_weights = tier2_weights_path(&storage_config, data_dir);
    // Persist `set_*` writes to the config's on-disk home under the data dir (or `MAILMATE_CONFIG`),
    // so a provider configured in one short-lived host process is still there for the next one.
    let router = build_router(
        &config,
        &backend,
        out,
        secrets,
        Some(resolve_config_path(data_dir)),
        Some(tier2_weights),
    )?;
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
/// `config_path` is where the admin `set_*` writes persist (production passes
/// `<data_dir>/config.toml`); `None` keeps the config in-memory for the session, which the
/// composition tests use so they touch no disk.
///
/// # Errors
/// [`RuntimeError::Storage`] if seeding a rule snapshot from the repository fails.
pub fn build_router(
    config: &AppConfig,
    backend: &Arc<SqliteBackend>,
    out: Arc<dyn Transport>,
    secrets_path: PathBuf,
    config_path: Option<PathBuf>,
    tier2_weights_path: Option<PathBuf>,
) -> Result<HostRouter, RuntimeError> {
    // --- Repositories (all over the one backend) ---
    let rules = Arc::new(SqliteRuleRepository::new(backend.clone()));
    let audit = Arc::new(SqliteAuditRepository::new(backend.clone()));
    let messages = Arc::new(SqliteMessageRepository::new(backend.clone()));
    let feedback = Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    let proposals = Arc::new(SqliteProposalRepository::new(backend.clone()));
    let conflicts = Arc::new(SqliteConflictRepository::new(backend.clone()));
    let items = Arc::new(SqlitePipelineItemRepository::new(backend.clone()));
    let workflows = Arc::new(SqliteWorkflowRepository::new(backend.clone()));
    let instances = Arc::new(SqliteWorkflowInstanceRepository::new(backend.clone()));
    let datasets = Arc::new(SqliteDatasetRepository::new(backend.clone()));
    let lora_adapters = Arc::new(SqliteAdapterRepository::new(backend.clone()));
    let eval_runs = Arc::new(SqliteEvalRunRepository::new(backend.clone()));
    let data_rights = Arc::new(SqliteDataRightsRepository::new(backend.clone()));
    let reminders = Arc::new(SqliteReminderRepository::new(backend.clone()));

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
    // The always-on online Tier-2 classifier (logistic regression) persists its learning to disk
    // when a path is configured (production), so a correction taught in one short-lived host
    // process is still learned by the next; the composition tests pass `None` to stay in-memory.
    let online_tier2: Arc<dyn Tier2Classifier> = Arc::new(match &tier2_weights_path {
        Some(path) => LogisticRegressionClassifier::with_persistence(
            TIER2_POSITIVE,
            TIER2_NEGATIVE,
            path.clone(),
        ),
        None => LogisticRegressionClassifier::new(TIER2_POSITIVE, TIER2_NEGATIVE),
    });
    // Phase 8: wrap the Tier-2 in a `SwappableTier2` so a freshly-trained, held-out-gated Burn
    // artifact can hot-swap it in place (no host restart). The candidate/active artifact dirs sit
    // beside the weights file; at startup a previously-activated artifact becomes the initial
    // backing, else the online model is the cold-start fallback.
    let tier2_model_dirs = tier2_weights_path.as_ref().map(|p| {
        let base = p.parent().map_or_else(
            || PathBuf::from("tier2_model"),
            |dir| dir.join("tier2_model"),
        );
        (base.join("candidate"), base.join("active"))
    });
    let initial_tier2: Arc<dyn Tier2Classifier> = tier2_model_dirs
        .as_ref()
        .and_then(|(_, active)| Tier2TrainingService::load_active(active))
        .unwrap_or(online_tier2);
    let swappable_tier2 = Arc::new(SwappableTier2::new(initial_tier2));
    let tier2: Arc<dyn Tier2Classifier> = swappable_tier2.clone();
    let tier2_training = tier2_model_dirs.map(|(candidate, active)| {
        Tier2TrainingService::new(
            swappable_tier2.clone(),
            classification_feedback.clone(),
            candidate,
            active,
            TIER2_POSITIVE,
            TIER2_NEGATIVE,
        )
        .with_tuning(config.tier2.max_rows, config.tier2.precision_gate)
    });
    // Build the configured provider from `[ai]` over the host's local HTTP transport. With no
    // provider configured (or an incomplete one), this degrades to UnavailableProvider — every
    // provider-typed collaborator still stands up and honestly reports "needs a provider".
    let http_client: Arc<dyn HttpClient> = Arc::new(StdHttpClient::new());
    // Wrap the configured provider in a `SwappableProvider` so a `set_provider`/`set_secret`
    // write from Settings can rebuild and hot-swap the backing adapter in place — no host restart.
    // Every provider-typed collaborator below holds this SAME handle, and the admin suite keeps a
    // clone to drive the swap.
    let live_provider = Arc::new(SwappableProvider::new(build_provider(
        &config.ai,
        secret_store.as_ref(),
        http_client.clone(),
    )));
    let provider: Arc<dyn AiProvider> = live_provider.clone();

    // --- Deterministic engines, seeded from the stored rule snapshots ---
    let classification_snapshot =
        block_on(rule_snapshot(rules.as_ref(), RuleKind::Classification))?;
    let action_snapshot = block_on(rule_snapshot(rules.as_ref(), RuleKind::Action))?;
    let mut all_rules = classification_snapshot.clone();
    all_rules.extend(action_snapshot.clone());

    let classification_rule_engine: Arc<dyn RuleEngine> =
        Arc::new(DeterministicRuleEngine::new(classification_snapshot));
    let action_rule_engine: Arc<dyn RuleEngine> =
        Arc::new(DeterministicRuleEngine::new(action_snapshot));
    let curator_rule_engine: Arc<dyn RuleEngine> =
        Arc::new(DeterministicRuleEngine::new(all_rules));

    // Keep handles to the three engines so the router can hot-reload them in place the moment a
    // rule is activated/materialized (without a host restart). They are the SAME Arc instances
    // the cascade/planner/curator hold, so reloading here updates what those see.
    let rule_reload = RuleReload {
        rules: rules.clone(),
        classification_engine: classification_rule_engine.clone(),
        action_engine: action_rule_engine.clone(),
        curator_engine: curator_rule_engine.clone(),
    };

    let classification_engine = Arc::new(CascadeClassifier::new(
        classification_rule_engine,
        tier2.clone(),
    ));
    let action_planner = Arc::new(DefaultActionPlanner::new(action_rule_engine));
    let policy_guard = Arc::new(HardPolicyGuard::new());

    // --- The learning loop + curator + review + drafter + training ---
    let learning_engine = Arc::new(
        DefaultLearningEngine::new(
            classification_feedback.clone(),
            filing_feedback.clone(),
            audit.clone(),
            proposals.clone(),
        )
        // Give it the rule snapshot so a proposal pass also surfaces human-gated retire
        // proposals for active rules the user keeps undoing (Phase 7 decay)…
        .with_rules(rules.clone())
        // …and the clock so it also surfaces retire proposals for rules that have gone *stale*
        // (no fires in the idle window) — the time-based half of Phase-7 decay.
        .with_clock(clock.clone()),
    );
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
        // Where those writes persist: `<data_dir>/config.toml` in production, `None` in tests.
        config_path,
        secret_store,
        // The shared HTTP transport, reused for `list_models` provider discovery.
        http: http_client,
        // The hot-swappable provider handle: a `set_provider`/`set_secret` write rebuilds from the
        // updated config + secrets and swaps the backing adapter in place — no host restart.
        live_provider: Some(live_provider),
    };

    let mut router = HostRouter::from_ports(&ports, audit, out)
        .with_followups(suite)
        .with_followup_batch_cap(config.followups.drain_batch_cap)
        .with_admin(admin)
        .with_rule_reload(rule_reload)
        .with_messages(messages)
        .with_data_rights(data_rights)
        .with_reminders(reminders, config.followups.reminder_batch_cap);
    if let Some(service) = tier2_training {
        router = router.with_tier2_training(service);
    }
    Ok(router)
}

/// Run the host: open the database, assemble the router over the stdio transport, drain
/// overdue follow-ups (catch-up-on-launch), optionally spawn the periodic worker, then serve
/// the loop until the peer hangs up.
///
/// # Errors
/// [`RuntimeError`] if the app cannot be built, the startup drain fails to emit, or the serve
/// loop ends on a fatal transport error.
pub fn serve(config: AppConfig, data_dir: &Path) -> Result<(), RuntimeError> {
    // The 0700 data-dir guarantee MUST land before anything else touches the directory: the
    // logger (next line) creates `host.log` inside it, which would otherwise create the dir at
    // the process umask and rob the backend of its chance to lock it. Create + lock here, once.
    ensure_dir_owner_only(data_dir)?;
    // Stand the file log up first so even a build_app failure is recorded. stderr is discarded by
    // the confined Thunderbird snap and stdout is the framed protocol, so the file is our only voice.
    let level = crate::logging::level_from_env();
    let log_path = crate::logging::init(data_dir, level);
    install_panic_logger();
    log::info!(
        "host starting: pid={} data_dir={} log={} level={level:?}",
        std::process::id(),
        data_dir.display(),
        log_path.display(),
    );

    let out: Arc<dyn Transport> = Arc::new(WriterTransport::new(stdout()));
    let app = build_app(config, data_dir, out)?;
    log::info!(
        "host ready: providers={} followups_tick={}s catch_up_on_launch={}",
        app.config.ai.providers.len(),
        app.config.followups.tick_seconds,
        app.config.followups.catch_up_on_launch,
    );

    // Seed the cold-start starter rules as drafts so a fresh install has something to review and
    // one-tap activate in session one. Idempotent (skips on a name collision), so it is safe to
    // run on every launch.
    match block_on(app.router.bootstrap_starter_rules()) {
        Ok(summary) => log::info!(
            "starter rules: {} seeded, {} already present",
            summary.imported,
            summary.skipped.len()
        ),
        Err(e) => log::warn!("starter-rule bootstrap failed: {e}"),
    }

    if app.config.followups.catch_up_on_launch {
        // Drain to empty at launch (batch-capped per pass) so a backlog accumulated while the host
        // was offline fully clears even when no periodic tick is configured (tick_seconds = 0).
        block_on(app.router.drain_followups_to_empty())?;
        block_on(app.router.drain_reminders_to_empty())?;
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
    match &result {
        Ok(()) => log::info!("host stopping: peer hung up (EOF)"),
        Err(e) => log::warn!("host stopping: transport error: {e}"),
    }

    // Tear the worker down before returning so the process exits cleanly on EOF.
    stop.store(true, Ordering::Relaxed);
    if let Some(handle) = ticker {
        let _ = handle.join();
    }
    result.map_err(RuntimeError::from)
}

/// Install a panic hook that records the panic to the host log before the default hook runs. A
/// panic inside an async handler (driven by `block_on`) otherwise vanishes — stderr is discarded —
/// leaving the channel to drop with no explanation in the log.
fn install_panic_logger() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        default(info);
    }));
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

/// The path the host loads its config from at startup and persists every `set_*` admin write
/// back to: `MAILMATE_CONFIG` if set, else `<data_dir>/config.toml`. Native-messaging hosts are
/// short-lived — Thunderbird re-spawns one per port, and an MV3 event page drops the port when it
/// suspends — so without an on-disk home a configured provider lives only in the now-dead process's
/// memory, which surfaced as a persistent "Provider: none" after the next reconnect. Keeping the
/// config beside the database and the secret store (all under the data dir) makes provider/settings
/// writes survive a restart.
#[must_use]
pub fn resolve_config_path(data_dir: &Path) -> PathBuf {
    env_nonempty("MAILMATE_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("config.toml"))
}

/// Resolve the effective config. An explicit `MAILMATE_CONFIG` is honored strictly (a named file
/// that fails to load/validate is an error). Otherwise the default home `<data_dir>/config.toml`
/// is loaded when it exists, falling back to the safe default on first run — a missing default-home
/// file is not an error, so a clean install just starts with zero providers.
///
/// # Errors
/// [`RuntimeError::Config`] if an explicit `MAILMATE_CONFIG` (or an *existing* default-home file)
/// fails to load or validate.
pub fn resolve_config() -> Result<AppConfig, RuntimeError> {
    match env_nonempty("MAILMATE_CONFIG") {
        Some(path) => Ok(AppConfig::load(Path::new(&path))?),
        None => {
            let path = resolve_data_dir().join("config.toml");
            if path.exists() {
                Ok(AppConfig::load(&path)?)
            } else {
                Ok(AppConfig::default())
            }
        }
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

/// The Tier-2 weights cache lives beside the database (or under the data dir for an in-memory
/// DB), so the always-on classifier's online learning survives a host restart.
fn tier2_weights_path(storage: &StorageConfig, data_dir: &Path) -> PathBuf {
    match &storage.path {
        StoragePath::File(db) => db.parent().map_or_else(
            || data_dir.join("tier2_weights.json"),
            |dir| dir.join("tier2_weights.json"),
        ),
        StoragePath::InMemory => data_dir.join("tier2_weights.json"),
    }
}

/// The active+shadow rule snapshot of `kind` across every scope — the immutable view the
/// deterministic engine evaluates. Empty on a fresh database (the zero-config posture).
pub(crate) async fn rule_snapshot(
    rules: &dyn RuleRepository,
    kind: RuleKind,
) -> Result<Vec<EvaluatableRule>, StorageError> {
    let mut snapshot = Vec::new();
    for scope in SCOPES {
        snapshot.extend(rules.get_active_rules(kind, scope).await?);
        snapshot.extend(rules.get_shadow_rules(kind, scope).await?);
    }
    Ok(snapshot)
}

/// The Rules-manager view of `kind`: the evaluated snapshot (active + shadow) **plus** disabled
/// rules across every scope. Disabled rules are absent from the engine snapshot, but the manager
/// must show them so a human can re-enable one they previously turned off.
pub(crate) async fn rule_manager_snapshot(
    rules: &dyn RuleRepository,
    kind: RuleKind,
) -> Result<Vec<EvaluatableRule>, StorageError> {
    // De-dup invariant: each rule has exactly ONE scope and ONE status, so the active+shadow
    // snapshot and the disabled query return disjoint sets — no rule appears twice. If rules ever
    // gain multiple scopes, switch to a `HashSet` keyed on `rule_id` here.
    let mut snapshot = rule_snapshot(rules, kind).await?;
    for scope in SCOPES {
        snapshot.extend(
            rules
                .get_rules_by_status(kind, scope, RuleStatus::Disabled)
                .await?,
        );
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
        let _ = block_on(router.drain_reminders());
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
            vec![1, 2, 3, 4, 5, 6, 7, 8]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_data_dir_is_locked_0700_before_the_logger_writes_into_it() {
        use std::os::unix::fs::PermissionsExt;
        // Reproduce the `serve` ordering: the guarantee must hold before anything (the logger)
        // creates files inside the data dir. A fresh, not-yet-existing data dir is created+locked
        // by `ensure_dir_owner_only`; then we mimic `logging::init` (create_dir_all + a file) and
        // confirm the mode is still owner-only — a file written into an already-0700 dir cannot
        // loosen it.
        let parent = tempfile::tempdir().unwrap();
        let data_dir = parent.path().join("mailmate");
        assert!(!data_dir.exists());

        ensure_dir_owner_only(&data_dir).unwrap();
        // What the logger does next:
        let _ = std::fs::create_dir_all(&data_dir);
        std::fs::write(data_dir.join("host.log"), b"start\n").unwrap();

        let mode = std::fs::metadata(&data_dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "got {:o}", mode & 0o777);
    }

    #[test]
    fn resolve_data_dir_prefers_the_explicit_env_override() {
        // Safe: this test owns the override and asserts only the override branch.
        std::env::set_var("MAILMATE_DATA_DIR", "/tmp/mailmate-test-dir");
        assert_eq!(resolve_data_dir(), PathBuf::from("/tmp/mailmate-test-dir"));
        std::env::remove_var("MAILMATE_DATA_DIR");
    }

    #[test]
    fn resolve_config_path_defaults_to_config_toml_under_the_data_dir() {
        // With no MAILMATE_CONFIG override, the config's on-disk home sits beside the database
        // under the data dir — the home that makes a configured provider survive a host restart.
        // (No test sets MAILMATE_CONFIG, so removing it here is safe and keeps the default branch.)
        std::env::remove_var("MAILMATE_CONFIG");
        assert_eq!(
            resolve_config_path(Path::new("/data/dir")),
            PathBuf::from("/data/dir/config.toml")
        );
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
            vec![1, 2, 3, 4, 5, 6, 7, 8]
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
