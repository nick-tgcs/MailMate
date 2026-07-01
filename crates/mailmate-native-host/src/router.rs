//! The host router: protocol frames in, core use-cases driven, frames out.
//!
//! [`HostRouter`] is the application seam Phase 10 adds — the translator between the
//! Thunderbird wire protocol and the core's use-cases. It owns the [`PlanningService`],
//! [`CorrectionService`], and [`DraftService`] (assembled from the injected [`Ports`]), the
//! [`MailClient`] it applies safe actions through, and the [`AuditRepository`] it records
//! provenance into. Every output frame leaves through one injected [`Transport`], so the
//! single-writer guard keeps responses, notifications, and mail commands from interleaving.
//!
//! The router names only ports and use-cases — no concrete engine, provider, or backend. The
//! Phase-12 composition root ([`crate::runtime`]) injects the real adapters in production; the
//! same router is exercised here against the in-memory fakes, with no change to the code here.
//!
//! ## What each request does
//! - `ping` — liveness echo (the Phase-1 contract, now served here too).
//! - `hello` — richer handshake: host/protocol version, wired capabilities, and the
//!   secret-free drafting/retention posture, so the extension can render Connected /
//!   version-mismatch before sending any real request.
//! - `classify_message` / `read_message` — classify a *selected* message and return the
//!   guarded plan as suggestions, each tagged with its `apply_state` (`suggest` here — a manual
//!   classify applies nothing; the user is in the loop).
//! - `new_mail` — classify a *background* arrival, **apply** the policy-allowed low-risk
//!   actions through the [`MailClient`], and push a `classification_ready` notification that
//!   surfaces the review-required ones. This is "apply safe actions returned by the host".
//! - `draft_reply` — generate an advisory, review-required draft.
//! - `record_user_action` — route a correction to its per-task feedback table, or a pure
//!   provenance/execution-result fact to the audit log — never to both. Beyond the original
//!   `junk_changed` / `message_moved` corrections, three learning discriminants:
//!   `classification_corrected` (a wrong-category fix → classification feedback),
//!   `action_undone` (an Undo of an auto-applied action → negative evidence against the rule
//!   that fired — a reverted move/junk reuses the filing/not-spam corrections; a reverted
//!   tag/draft has no feedback table and is audited), and `suggestion_dismissed` (the
//!   ignore-rate signal — audited, never fabricated into a feedback row it has no chosen
//!   label/folder for).

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use serde_json::{json, Value};

use mailmate_ai::http::HttpClient;
use mailmate_ai::providers::{list_models, SwappableProvider};
use mailmate_common::action::{GuardedActionPlan, PlannedAction};
use mailmate_common::actor::Actor;
use mailmate_common::audit::{event_type, AuditEntry, AuditQuery};
use mailmate_common::classification::Classification;
use mailmate_common::correction::UserCorrection;
use mailmate_common::curator::ReviewDecision;
use mailmate_common::error::TransportError;
use mailmate_common::features::FeatureVector;
use mailmate_common::feedback::{
    FeedbackPolarity, FilingFeedback, FilingFeedbackRow, PinnedVersions, TaskFeedback,
};
use mailmate_common::ids::{
    AccountId, FolderId, MessageId, ProposalId, ReminderId, RuleId, ThreadId,
};
use mailmate_common::mail::MailAction;
use mailmate_common::message::NewMessage;
use mailmate_common::pipeline::PipelineItemQuery;
use mailmate_common::policy::TriggerKind;
use mailmate_common::proposal::{ProposalStatus, ProposalTrigger};
use mailmate_common::protocol::{Frame, ProtocolVersion};
use mailmate_common::reminder::NewReminder;
use mailmate_common::reply::ReplyDraftRequest;
use mailmate_common::retention::RetentionLevel;
use mailmate_common::rules::rule::{RuleKind, RuleStatus};
use mailmate_common::secret::{Secret, SecretKey};
use mailmate_common::time::Timestamp;
use mailmate_common::workflow::{ExitEvent, WorkflowInstanceStatus};
use mailmate_core::{
    CorrectionContext, CorrectionService, DraftService, ImportExportService, ImportSummary,
    PlanningOutcome, PlanningService, Ports,
};
use mailmate_ports::clock::Clock;
use mailmate_ports::exit_detector::ExitDetector;
use mailmate_ports::feature_extractor::FeatureExtractor;
use mailmate_ports::follow_up_scheduler::FollowUpScheduler;
use mailmate_ports::learning_engine::LearningEngine;
use mailmate_ports::mail_client::MailClient;
use mailmate_ports::proposal_review::ProposalReview;
use mailmate_ports::rule_engine::RuleEngine;
use mailmate_ports::secret_store::SecretStore;
use mailmate_ports::storage::data_rights::DataRightsRepository;
use mailmate_ports::storage::messages::MessageRepository;
use mailmate_ports::storage::pipeline_items::PipelineItemRepository;
use mailmate_ports::storage::reminders::ReminderRepository;
use mailmate_ports::storage::rules::RuleRepository;
use mailmate_ports::storage::workflows::WorkflowInstanceRepository;
use mailmate_ports::storage::{AuditRepository, ProposalRepository};
use mailmate_ports::transport::Transport;
use mailmate_ports::workflow_engine::WorkflowEngine;

use crate::config::{AppConfig, CategoryPolicy, ProviderSettings};
use crate::convert::{
    classification_ready_payload, classify_response_payload, conflicts_json,
    followup_draft_ready_payload, followup_needs_attention_payload, proposal_ready_payload,
    AppliedAction,
};
use crate::dispatch::{error_response, ok_response, SUPPORTED_PROTOCOL_VERSION};
use crate::native_stdio::read_frame;
use crate::protocol_dto::{
    CancelSequencePayload, ClassifyMessagePayload, DraftReplyPayload, EnrollPipelineItemPayload,
    ExplainDecisionPayload, ListFollowupsPayload, ListRecentActivityPayload,
    RecordUserActionPayload, RegenerateDraftPayload, RescheduleFollowupPayload,
    ReviewFollowupPayload, ReviewRuleProposalPayload, SentMailPayload, TriageExistingMailPayload,
    UpdatePipelineStagePayload,
};
use crate::provider::provider_is_configured;

/// The follow-up engine + repository ports the router needs to serve the sales-pipeline
/// control requests and drain due steps. They live outside the core's [`Ports`] (the core
/// never schedules), so the host injects them as a bundle. A router built without them
/// answers the follow-up request types with `followups_not_configured`.
#[derive(Clone)]
pub struct FollowUpSuite {
    /// The pipeline-item store (enroll / reply-exit lookups, and the `list_followups` read).
    pub pipeline_items: Arc<dyn PipelineItemRepository>,
    /// The workflow-instance store — read directly by `list_followups` to join each deal to its
    /// instance status / next-due step.
    pub instances: Arc<dyn WorkflowInstanceRepository>,
    /// Arms instances, reschedules, resolves reviews, detects conflicts.
    pub workflow_engine: Arc<dyn WorkflowEngine>,
    /// The catch-up-on-launch drain.
    pub scheduler: Arc<dyn FollowUpScheduler>,
    /// Reply / won / lost / cancel exit handling.
    pub exit_detector: Arc<dyn ExitDetector>,
}

/// The read-only management surface the host exposes for the review/explanation UI: the
/// proposal store (pending reviews) and a secret-free settings snapshot. Like the follow-up
/// suite, it lives outside the core's [`Ports`] and is injected by the composition root; a
/// router without it answers the admin request types with `admin_not_configured`.
#[derive(Clone)]
pub struct AdminSuite {
    /// The agent-proposal store (for the pending-review queue).
    pub proposals: Arc<dyn ProposalRepository>,
    /// The live, mutable host config — the source of truth `get_settings` reads and the `set_*`
    /// writes mutate. Behind a `Mutex` so writes are serialized with reads.
    pub config: Arc<Mutex<AppConfig>>,
    /// Where the config persists, if it came from a file (`MAILMATE_CONFIG`). `None` means an
    /// in-memory/default config: writes still take effect for the session but are not saved, and
    /// the write responses say so honestly.
    pub config_path: Option<PathBuf>,
    /// The 0600 secret store — the only place a provider API key is written (via `set_secret`).
    pub secret_store: Arc<dyn SecretStore>,
    /// The host's HTTP transport, reused for provider model discovery (`list_models`). It lives
    /// here so the management surface can probe a provider's catalog endpoint *before* a model is
    /// chosen — model discovery cannot wait for a fully-built provider (which already needs one).
    pub http: Arc<dyn HttpClient>,
    /// The live, hot-swappable provider every provider-typed collaborator holds (drafter, curator,
    /// training evaluator). A `set_provider`/`set_secret` write rebuilds from the updated config +
    /// secrets and swaps the backing adapter in here, so the change takes effect on the next draft
    /// with no host restart. `None` in a router without a live provider graph (e.g. admin-only
    /// tests), where settings writes simply skip the rebuild.
    pub live_provider: Option<Arc<SwappableProvider>>,
}

impl AdminSuite {
    /// The current secret-free settings snapshot, derived live from the mutable config.
    fn snapshot(&self) -> crate::config::SettingsSnapshot {
        self.config.lock().unwrap().settings_snapshot()
    }

    /// The 0600 secret-store key holding a provider's API key.
    fn secret_key(provider_id: &str) -> SecretKey {
        SecretKey::from(format!("{provider_id}_api_key").as_str())
    }

    /// Whether a provider has an API key in the store — presence only, never the value.
    /// `Some(true/false)` is a clean present/absent answer; `None` means the store could not be
    /// read (corrupt/unreadable secrets file), which must not be reported as a confident "absent".
    async fn secret_is_set(&self, provider_id: &str) -> Option<bool> {
        match self.secret_store.get(Self::secret_key(provider_id)).await {
            Ok(value) => Some(value.is_some()),
            Err(_) => None,
        }
    }

    /// Rebuild the live provider from the current `[ai]` config + stored secrets and hot-swap it
    /// into every provider-typed collaborator in place (no host restart). Called after a
    /// `set_provider`/`set_secret` write succeeds, so a provider added or keyed from Settings is
    /// used by the very next `draft_reply` — closing the gap where `get_settings` reported the new
    /// default while the drafter/curator/training evaluator still used the old (often unavailable)
    /// adapter until a restart. A no-op when no live provider handle is wired (an admin-only test
    /// router). `build_provider` reads secrets synchronously, mirroring `provider_status`, which
    /// already does so from its async handler.
    fn rebuild_live_provider(&self) {
        let Some(live) = &self.live_provider else {
            return;
        };
        let ai = self.config.lock().unwrap().ai.clone();
        let next =
            crate::provider::build_provider(&ai, self.secret_store.as_ref(), self.http.clone());
        log::info!(
            "rebuilt live provider after a settings write: now serving id={}",
            next.id()
        );
        live.swap(next);
    }

    /// Resolve the `(kind, endpoint, api_key)` a discovery/probe request targets: from an explicit
    /// `kind` + `endpoint` in the payload, falling back to a saved provider's settings when only a
    /// `provider_id` is given. Shared by `list_models` and `test_provider` so both resolve a target
    /// identically. Returns the human-readable tail of the error the caller surfaces when neither
    /// path yields a kind + non-empty endpoint.
    ///
    /// The stored API key is bound to the provider's **own saved endpoint**: it is attached only
    /// when the resolved endpoint is exactly that saved endpoint, never to a payload `endpoint`
    /// override (see the SSRF / key-exfiltration guard below).
    async fn resolve_discovery_target(
        &self,
        payload: &Value,
    ) -> Result<(String, String, Option<Secret>), String> {
        let provider_id = payload
            .get("provider_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        // The short-lived clone drops the config lock before the `.await` below — never hold a
        // `std::sync` guard across an await point.
        let saved = provider_id.and_then(|id| {
            self.config
                .lock()
                .unwrap()
                .ai
                .providers
                .iter()
                .find(|p| p.id == id)
                .cloned()
        });
        let kind = payload
            .get("kind")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| saved.as_ref().map(|p| p.kind.clone()));
        let saved_endpoint = saved.as_ref().and_then(|p| p.endpoint.clone());
        let endpoint = payload
            .get("endpoint")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| saved_endpoint.clone());
        let (Some(kind), Some(endpoint)) = (kind, endpoint) else {
            return Err(
                "requires kind and endpoint (or a saved provider_id that has them)".to_owned(),
            );
        };
        if endpoint.trim().is_empty() {
            return Err("requires a non-empty endpoint".to_owned());
        }
        // SSRF / key-exfiltration guard: the stored key is bound to the provider's OWN saved
        // endpoint. Attach it ONLY when the resolved endpoint is exactly that saved endpoint —
        // never to a payload `endpoint` override. Otherwise a crafted request like
        // `{ provider_id: "openai", endpoint: "https://attacker.example/v1" }` would send
        // `Authorization: Bearer <saved key>` to an arbitrary host. Probing a different (or
        // not-yet-saved) endpoint still works for discovery — just without the key. A store read
        // error degrades to "no key" (a local catalog needs none).
        let targets_saved_endpoint = saved_endpoint
            .as_deref()
            .is_some_and(|saved| saved.trim() == endpoint.trim());
        let api_key = match provider_id {
            Some(id) if targets_saved_endpoint => self
                .secret_store
                .get(Self::secret_key(id))
                .await
                .ok()
                .flatten(),
            _ => None,
        };
        Ok((kind, endpoint, api_key))
    }

    /// Persist the live config to disk if it came from a file. `Ok(false)` means an in-memory
    /// config (the write took effect for the session but was not saved).
    fn persist(&self) -> Result<bool, String> {
        match &self.config_path {
            Some(path) => self
                .config
                .lock()
                .unwrap()
                .save(path)
                .map(|()| true)
                .map_err(|e| e.to_string()),
            None => Ok(false),
        }
    }
}

/// The handles the router needs to hot-reload the deterministic rule engines in place the moment
/// the active/shadow rule set changes — e.g. a human activates a learned rule. It carries the
/// rule store (to re-read the snapshot) and the three engine handles built by the composition
/// root (the SAME `Arc`s the cascade/planner/curator hold, so reloading updates what they see).
/// Injected by [`HostRouter::with_rule_reload`]; absent in a router built without the composition
/// root (its `accept_active` still activates the rule in the DB, it just won't fire until the
/// next host start — the composition tests that care wire this).
#[derive(Clone)]
pub struct RuleReload {
    /// The rule store, re-read to rebuild the active+shadow snapshot.
    pub rules: Arc<dyn RuleRepository>,
    /// The classification cascade's deterministic rule engine.
    pub classification_engine: Arc<dyn RuleEngine>,
    /// The action planner's deterministic rule engine.
    pub action_engine: Arc<dyn RuleEngine>,
    /// The curator's combined-snapshot rule engine (used for conflict detection).
    pub curator_engine: Arc<dyn RuleEngine>,
}

/// How many recently-classified messages' feature vectors the router keeps so a correction
/// can be learned against the SAME features the classifier saw (not an empty vector). Bounded
/// — a long session evicts oldest-first; a correction whose message has aged out simply learns
/// with no extra features (the prior, degraded behaviour), never a wrong vector.
const FEATURE_CACHE_CAP: usize = 1024;

/// A bounded, FIFO-evicting cache of `MessageId → FeatureVector`, captured at classification
/// time and recalled at correction time so every feedback row records the full feature vector
/// (§3.1) rather than just the injected sender domain.
#[derive(Default)]
struct FeatureCache {
    map: HashMap<MessageId, FeatureVector>,
    /// The account each cached message belongs to, retained on the same LRU discipline so a later
    /// correction can scope a learned rule per account even when the extension omits `account_id`.
    accounts: HashMap<MessageId, AccountId>,
    order: VecDeque<MessageId>,
}

impl FeatureCache {
    fn insert(&mut self, id: MessageId, features: FeatureVector, account: AccountId) {
        self.accounts.insert(id.clone(), account);
        if self.map.insert(id.clone(), features).is_none() {
            self.order.push_back(id);
        }
        while self.order.len() > FEATURE_CACHE_CAP {
            if let Some(evicted) = self.order.pop_front() {
                self.map.remove(&evicted);
                self.accounts.remove(&evicted);
            }
        }
    }

    fn get(&self, id: &MessageId) -> Option<FeatureVector> {
        self.map.get(id).cloned()
    }

    fn account(&self, id: &MessageId) -> Option<AccountId> {
        self.accounts.get(id).cloned()
    }
}

/// Routes protocol frames into the core and emits the resulting frames.
#[derive(Clone)]
pub struct HostRouter {
    planning: PlanningService,
    correction: CorrectionService,
    draft: DraftService,
    learning: Arc<dyn LearningEngine>,
    mail_client: Arc<dyn MailClient>,
    audit: Arc<dyn AuditRepository>,
    clock: Arc<dyn Clock>,
    proposal_review: Arc<dyn ProposalReview>,
    feature_extractor: Arc<dyn FeatureExtractor>,
    recent_features: Arc<Mutex<FeatureCache>>,
    out: Arc<dyn Transport>,
    followups: Option<FollowUpSuite>,
    admin: Option<AdminSuite>,
    rule_reload: Option<RuleReload>,
    messages: Option<Arc<dyn MessageRepository>>,
    tier2_training: Option<crate::tier2_training::Tier2TrainingService>,
    data_rights: Option<Arc<dyn DataRightsRepository>>,
    reminders: Option<Arc<dyn ReminderRepository>>,
    reminder_batch_cap: usize,
    followup_batch_cap: usize,
}

/// The default ceiling on how many due reminders one drain nudges — a long offline gap surfaces
/// in capped batches over successive ticks rather than an unbounded notification storm.
const DEFAULT_REMINDER_BATCH_CAP: usize = 50;

/// The default ceiling on how many due follow-up workflow instances one drain fires.
const DEFAULT_FOLLOWUP_BATCH_CAP: usize = 100;

/// A hard backstop on launch-catch-up iterations: each pass drains one batch cap, so this bounds
/// the worst-case catch-up to `MAX_CATCH_UP_PASSES × cap` items and guarantees termination even if
/// a pass somehow keeps re-selecting (it should not). Far above any realistic offline backlog.
const MAX_CATCH_UP_PASSES: usize = 10_000;

impl HostRouter {
    /// Assemble the router from the core [`Ports`], the audit sink, and the output transport.
    /// The follow-up control requests are inert until [`with_followups`](Self::with_followups)
    /// supplies the workflow engine/scheduler/exit-detector + pipeline-item store.
    #[must_use]
    pub fn from_ports(
        ports: &Ports,
        audit: Arc<dyn AuditRepository>,
        out: Arc<dyn Transport>,
    ) -> Self {
        Self {
            planning: PlanningService::from_ports(ports),
            correction: CorrectionService::from_ports(ports),
            draft: DraftService::from_ports(ports),
            learning: ports.learning_engine.clone(),
            mail_client: ports.mail_client.clone(),
            audit,
            clock: ports.clock.clone(),
            proposal_review: ports.proposal_review.clone(),
            feature_extractor: ports.feature_extractor.clone(),
            recent_features: Arc::new(Mutex::new(FeatureCache::default())),
            out,
            followups: None,
            admin: None,
            rule_reload: None,
            messages: None,
            tier2_training: None,
            data_rights: None,
            reminders: None,
            reminder_batch_cap: DEFAULT_REMINDER_BATCH_CAP,
            followup_batch_cap: DEFAULT_FOLLOWUP_BATCH_CAP,
        }
    }

    /// Capture a just-classified message's features so a later correction on it learns against
    /// the same vector the classifier saw. Keyed by the internal message id.
    fn cache_features(&self, message: &mailmate_common::mail::MessageData) {
        if let Some(id) = &message.id {
            let features = self.feature_extractor.extract(message);
            if let Ok(mut cache) = self.recent_features.lock() {
                cache.insert(id.clone(), features, message.account_id.clone());
            }
        }
    }

    /// The cached feature vector for `message_id`, or an empty one when the message was never
    /// classified by this host or has aged out of the cache (degrades, never lies).
    fn cached_features(&self, message_id: &MessageId) -> FeatureVector {
        self.recent_features
            .lock()
            .ok()
            .and_then(|cache| cache.get(message_id))
            .unwrap_or_default()
    }

    /// The account of a just-classified `message_id`, when still cached — the fallback source of
    /// `account_id` for a correction whose payload omits it (so a learned rule can still scope to
    /// the account). `None` once the message ages out: induction then degrades to `Global`.
    fn cached_account(&self, message_id: &MessageId) -> Option<AccountId> {
        self.recent_features
            .lock()
            .ok()
            .and_then(|cache| cache.account(message_id))
    }

    /// Wire the follow-up engine suite, enabling the sales-pipeline control requests and the
    /// [`drain_followups`](Self::drain_followups) sweep.
    #[must_use]
    pub fn with_followups(mut self, followups: FollowUpSuite) -> Self {
        self.followups = Some(followups);
        self
    }

    /// Override the follow-up drain batch cap (how many due instances one drain fires). A zero
    /// or unset value keeps the default.
    #[must_use]
    pub fn with_followup_batch_cap(mut self, cap: usize) -> Self {
        if cap > 0 {
            self.followup_batch_cap = cap;
        }
        self
    }

    /// Wire the management surface, enabling `list_pending_reviews` and `get_settings`.
    #[must_use]
    pub fn with_admin(mut self, admin: AdminSuite) -> Self {
        self.admin = Some(admin);
        self
    }

    /// Wire the rule-reload handles, enabling [`reload_rules`](Self::reload_rules) to hot-reload
    /// the engines in place after a rule is activated/materialized (so it fires without a host
    /// restart).
    #[must_use]
    pub fn with_rule_reload(mut self, rule_reload: RuleReload) -> Self {
        self.rule_reload = Some(rule_reload);
        self
    }

    /// Wire the message store, enabling intake persistence (a background arrival is stored, its
    /// body gated by the effective retention level) and the down-level body purge on a retention
    /// downgrade. A router without it simply persists no messages (the prior behaviour).
    #[must_use]
    pub fn with_messages(mut self, messages: Arc<dyn MessageRepository>) -> Self {
        self.messages = Some(messages);
        self
    }

    /// Wire the on-device Tier-2 training service (Phase 8), enabling `train_tier2_model` to
    /// train a Burn classifier from corrections, gate it on held-out precision, and hot-swap it
    /// into the cascade. A router without it answers `train_tier2_model` with not-configured.
    #[must_use]
    pub fn with_tier2_training(
        mut self,
        service: crate::tier2_training::Tier2TrainingService,
    ) -> Self {
        self.tier2_training = Some(service);
        self
    }

    /// Wire the data-rights repository (Phase 9 delete-my-data / export), enabling
    /// `forget_message`, `forget_sender`, `reset_learning`, and `export_my_data`. A router
    /// without it answers those requests with not-configured.
    #[must_use]
    pub fn with_data_rights(mut self, data_rights: Arc<dyn DataRightsRepository>) -> Self {
        self.data_rights = Some(data_rights);
        self
    }

    /// Wire the reminder repository (Phase 9 snooze / durable remind-me), enabling `remind_me`,
    /// `snooze_reminder`, `cancel_reminder`, `list_reminders`, and the notify-only
    /// [`drain_reminders`](Self::drain_reminders). `batch_cap` bounds how many due reminders one
    /// drain nudges (a zero or unset cap falls back to the default). A router without it answers
    /// those requests with not-configured and its reminder drain is a no-op.
    #[must_use]
    pub fn with_reminders(
        mut self,
        reminders: Arc<dyn ReminderRepository>,
        batch_cap: usize,
    ) -> Self {
        self.reminders = Some(reminders);
        if batch_cap > 0 {
            self.reminder_batch_cap = batch_cap;
        }
        self
    }

    /// Persist a just-arrived message to the store, with its readable body gated by the EFFECTIVE
    /// retention level (the repository makes the body call, never the caller — see
    /// [`MessageRepository::insert`]). Best-effort: a no-op when the store is unwired or the
    /// message has no internal id, and a logged warning (never a dropped arrival) on a write
    /// failure. The retention default is full-body, so "we retain bodies" is not vacuous — but a
    /// metadata posture stores identity only.
    async fn persist_intake(&self, message: &mailmate_common::mail::MessageData) {
        let Some(messages) = &self.messages else {
            return;
        };
        let Some(id) = message.id.clone() else {
            return;
        };
        let retention = self.admin.as_ref().map_or(RetentionLevel::Metadata, |a| {
            a.config.lock().unwrap().retention_level()
        });
        let from = &message.headers.from;
        let sender_domain = from
            .rsplit_once('@')
            .map_or_else(String::new, |(_, d)| d.trim().to_ascii_lowercase());
        let now = self.clock.now();
        let new_message = NewMessage {
            id,
            account_id: message.account_id.clone(),
            folder_id: message.folder_id.clone(),
            thunderbird_message_id: message.client_message_id.clone(),
            rfc_message_id_hash: None,
            thread_id: message.thread_id.clone(),
            sender_email: from.clone(),
            sender_domain,
            subject: message.headers.subject.clone(),
            // The message's actual receipt time when its `Date` parsed, falling back to now only
            // for a missing/malformed header — so the stored chronology is honest, not the
            // moment we happened to persist it.
            received_at: message.headers.date.unwrap_or(now),
            body_hash: None,
            // The candidate body; the repository persists it ONLY when `retention.retains_body()`.
            body_text: message.body_text.clone(),
            retention,
            created_at: now,
        };
        if let Err(e) = messages.insert(new_message).await {
            log::warn!("intake message persist failed: {e}");
        }
    }

    /// The down-level purge: when the EFFECTIVE retention no longer retains bodies, erase every
    /// already-stored body so "turn the dial down and the bodies are gone" is true immediately.
    /// Idempotent (purges 0 when nothing is stored or the store is unwired); `body_hash` is kept.
    async fn purge_bodies_if_not_retaining(&self, admin: &AdminSuite) {
        let Some(messages) = &self.messages else {
            return;
        };
        if admin
            .config
            .lock()
            .unwrap()
            .retention_level()
            .retains_body()
        {
            return;
        }
        match messages.purge_bodies().await {
            Ok(n) if n > 0 => log::info!("retention lowered: purged {n} stored message bodies"),
            Ok(_) => {}
            Err(e) => log::warn!("body purge after retention change failed: {e}"),
        }
    }

    /// Re-read the active+shadow rule snapshot and hot-reload the deterministic engines in place,
    /// so a just-activated (or status-changed) rule takes effect on the very next classification —
    /// in the SAME host process, no restart. A no-op when rule-reload was not wired.
    ///
    /// # Errors
    /// [`StorageError`](mailmate_common::error::StorageError) if re-reading the snapshot fails.
    pub async fn reload_rules(&self) -> Result<(), mailmate_common::error::StorageError> {
        let Some(reload) = &self.rule_reload else {
            return Ok(());
        };
        let classification =
            crate::runtime::rule_snapshot(reload.rules.as_ref(), RuleKind::Classification).await?;
        let action = crate::runtime::rule_snapshot(reload.rules.as_ref(), RuleKind::Action).await?;
        reload.classification_engine.reload(classification.clone());
        reload.action_engine.reload(action.clone());
        // The curator engine evaluates the combined set for conflict detection.
        let mut combined = classification;
        combined.extend(action);
        reload.curator_engine.reload(combined);
        Ok(())
    }

    /// Drive the host loop synchronously: read a frame, [`handle`](Self::handle) it (blocking
    /// on the async use-cases — native messaging is a serial request/response stream), and
    /// continue until the peer hangs up. A malformed inbound frame is answered with a
    /// `malformed_frame` error and the loop continues (the codec stays frame-aligned).
    ///
    /// # Errors
    /// Returns the fatal [`TransportError`] that ended the loop (an I/O failure or a desynced
    /// stream), if any.
    pub fn serve_blocking<R: Read>(&self, reader: &mut R) -> Result<(), TransportError> {
        loop {
            match read_frame(reader) {
                Ok(None) => return Ok(()),
                Ok(Some(frame)) => block_on(self.handle(frame))?,
                Err(TransportError::Codec(message)) => {
                    self.send(error_response(
                        String::new(),
                        "malformed_frame",
                        message,
                        None,
                    ))?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Handle one inbound frame, emitting the response and/or notification(s) it produces.
    ///
    /// # Errors
    /// Propagates a [`TransportError`] from the output transport. A *handler* failure (bad
    /// payload, a use-case error) is turned into an error **response frame**, not an `Err` —
    /// the channel stays alive.
    pub async fn handle(&self, frame: Frame) -> Result<(), TransportError> {
        match frame {
            Frame::Request {
                protocol_version,
                request_id,
                type_,
                payload,
            } => {
                if protocol_version.0 != SUPPORTED_PROTOCOL_VERSION {
                    let received = protocol_version.0;
                    return self.send(error_response(
                        request_id,
                        "unsupported_protocol_version",
                        format!("unsupported protocol version {received:?}"),
                        Some(json!({ "supported": SUPPORTED_PROTOCOL_VERSION, "received": received })),
                    ));
                }
                self.route(&type_, request_id, payload).await
            }
            Frame::Response { request_id, .. } => self.send(error_response(
                request_id,
                "unexpected_kind",
                "host received a response frame; it only accepts requests",
                None,
            )),
            Frame::Notification {
                notification_id, ..
            } => self.send(error_response(
                notification_id,
                "unexpected_kind",
                "host received a notification frame; it only accepts requests",
                None,
            )),
        }
    }

    /// Route a version-checked request by its `type`.
    async fn route(
        &self,
        type_: &str,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        log::debug!("request type={type_} id={request_id}");
        match type_ {
            "ping" => self.send(ok_response(
                request_id,
                json!({ "pong": true, "echo": payload.get("nonce").cloned().unwrap_or(Value::Null) }),
            )),
            "hello" => self.handle_hello(request_id),
            "classify_message" | "read_message" => self.handle_classify(request_id, payload).await,
            "new_mail" => self.handle_new_mail(payload).await,
            "triage_existing_mail" => self.handle_triage_existing(request_id, payload).await,
            "draft_reply" => self.handle_draft(request_id, payload).await,
            "regenerate_draft" => self.handle_regenerate(request_id, payload).await,
            "record_user_action" => self.handle_record(request_id, payload).await,
            "record_sent_mail" => self.handle_record_sent_mail(request_id, payload).await,
            "enroll_pipeline_item" => self.handle_enroll(request_id, payload).await,
            "update_pipeline_stage" => self.handle_update_stage(request_id, payload).await,
            "cancel_sequence" => self.handle_cancel(request_id, payload).await,
            "reschedule_followup" => self.handle_reschedule(request_id, payload, false).await,
            "snooze" => self.handle_reschedule(request_id, payload, true).await,
            "review_followup" => self.handle_review_followup(request_id, payload).await,
            "list_followups" => self.handle_list_followups(request_id, payload).await,
            "explain_decision" => self.handle_explain(request_id, payload).await,
            "list_recent_activity" => self.handle_list_recent_activity(request_id, payload).await,
            "list_pending_reviews" => self.handle_list_reviews(request_id).await,
            "review_rule_proposal" => self.handle_review_proposal(request_id, payload).await,
            "list_rules" => self.handle_list_rules(request_id).await,
            "set_rule_status" => self.handle_set_rule_status(request_id, payload).await,
            "train_tier2_model" => self.handle_train_tier2(request_id).await,
            "forget_message" => self.handle_forget_message(request_id, payload).await,
            "forget_sender" => self.handle_forget_sender(request_id, payload).await,
            "reset_learning" => self.handle_reset_learning(request_id).await,
            "export_my_data" => self.handle_export_my_data(request_id).await,
            "remind_me" => self.handle_remind_me(request_id, payload).await,
            "snooze_reminder" => self.handle_snooze_reminder(request_id, payload).await,
            "cancel_reminder" => self.handle_cancel_reminder(request_id, payload).await,
            "list_reminders" => self.handle_list_reminders(request_id, payload).await,
            "get_settings" => self.handle_get_settings(request_id).await,
            "set_settings" => self.handle_set_settings(request_id, payload).await,
            "set_category_policy" => self.handle_set_category_policy(request_id, payload).await,
            "set_account_scope" => self.handle_set_account_scope(request_id, payload).await,
            "set_tag_mapping" => self.handle_set_tag_mapping(request_id, payload).await,
            "set_pause" => self.handle_set_pause(request_id, payload).await,
            "set_secret" => self.handle_set_secret(request_id, payload).await,
            "set_provider" => self.handle_set_provider(request_id, payload).await,
            "list_models" => self.handle_list_models(request_id, payload).await,
            "provider_status" => self.handle_provider_status(request_id).await,
            "test_provider" => self.handle_test_provider(request_id, payload).await,
            other => {
                log::warn!("unknown request type {other:?} id={request_id}");
                self.send(error_response(
                    request_id,
                    "unknown_request_type",
                    format!("unknown request type {other:?}"),
                    Some(json!({ "type": other })),
                ))
            }
        }
    }

    /// Classify a selected message and return the guarded plan as suggestions (no apply).
    ///
    /// The per-category / per-account triage policy is **not** applied here: this is a synchronous,
    /// user-initiated "tell me about THIS message" request (and it never auto-applies anyway), so
    /// the verdict and its suggestions are always shown. The policy governs the AUTONOMOUS
    /// background path ([`handle_new_mail`](Self::handle_new_mail)) — what MailMate does unasked.
    async fn handle_classify(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let parsed: ClassifyMessagePayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        let tb_id = parsed.thunderbird_message_id.clone();
        let message = parsed.into_message_data();
        // Remember the features so a later correction on this message learns the full vector.
        self.cache_features(&message);
        // Parse the one-click unsubscribe affordance from the headers before the message is moved.
        let unsubscribe = mailmate_common::unsubscribe::parse_unsubscribe(
            message.headers.list_unsubscribe.as_deref(),
            message.headers.list_unsubscribe_post.as_deref(),
        );
        match self
            .planning
            .handle_message(message, TriggerKind::NewMail)
            .await
        {
            Ok(outcome) => {
                let mut payload = classify_response_payload(&outcome, &tb_id);
                if let Some(unsub) = unsubscribe {
                    payload["unsubscribe"] = serde_json::to_value(unsub).unwrap_or(Value::Null);
                }
                self.send(ok_response(request_id, payload))
            }
            Err(e) => self.send(error_response(
                request_id,
                "classification_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Classify a background arrival, apply the allowed low-risk actions, and push the result.
    async fn handle_new_mail(&self, payload: Value) -> Result<(), TransportError> {
        let parsed: ClassifyMessagePayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            // A malformed background event has no request to correlate to; record it and drop.
            Err(e) => {
                let _ = self
                    .audit
                    .append(
                        AuditEntry::new("new_mail_rejected", Actor::Extension)
                            .with_payload(json!({ "error": e.to_string() })),
                    )
                    .await;
                return Ok(());
            }
        };
        let tb_id = parsed.thunderbird_message_id.clone();
        let message = parsed.into_message_data();
        // Capture the header metadata before the message is consumed, so the dashboard Review
        // card can show the real subject/sender (header metadata, always within `metadata`
        // retention — never body content).
        let subject = message.headers.subject.clone();
        let from = message.headers.from.clone();
        // The folder the message arrives in is the origin a move's Undo reverses to.
        let origin_folder = message.folder_id.clone();
        // The account this arrived on — read before the message is consumed below — drives the
        // per-account triage scope (an out-of-scope account is silenced like a category `Off`).
        let account_id = message.account_id.clone();
        // Remember the features so a later correction on this message learns the full vector.
        self.cache_features(&message);
        // Persist the arrival to the message store (body gated by the effective retention level),
        // so the retention dial is meaningful and the store can feed future learning/back-tests.
        self.persist_intake(&message).await;
        let outcome = match self.planning.handle_new_mail(message).await {
            Ok(o) => o,
            Err(e) => {
                let _ = self
                    .audit
                    .append(
                        AuditEntry::new("classification_failed", Actor::System).with_payload(
                            json!({ "thunderbird_message_id": tb_id, "error": e.to_string() }),
                        ),
                    )
                    .await;
                return Ok(());
            }
        };
        // Fold the global pause and the per-category / per-account triage policy into the effective
        // posture for THIS arrival (the strictest wins). `Auto` keeps the active-rule auto-apply
        // path; `Suggest` auto-applies nothing but surfaces every action as a suggestion (so a
        // paused or suggest-only category is honest — the actions are visible, just not taken);
        // `Off` silences the arrival entirely (no apply, no suggestions). A policy change applies
        // to NEW arrivals only — already-applied actions are never retroactively revoked.
        let policy = self.triage_policy_for(&account_id, &outcome.classification);
        let (outcome, applied) = match policy {
            CategoryPolicy::Off => (silence_actions(outcome), Vec::new()),
            CategoryPolicy::Suggest => (demote_to_suggestions(outcome), Vec::new()),
            CategoryPolicy::Auto => {
                let applied = self
                    .apply_allowed(&outcome.guarded_plan, &origin_folder)
                    .await;
                (outcome, applied)
            }
        };
        let payload = classification_ready_payload(&outcome, &tb_id, &applied, &subject, &from);
        self.send(Frame::Notification {
            protocol_version: ProtocolVersion::default(),
            notification_id: format!("ntf_classify_{tb_id}"),
            type_: "classification_ready".to_owned(),
            payload,
        })
    }

    /// The first-run backfill: triage a page of already-present messages WITHOUT touching mail.
    /// Each message is classified as a dry run (nothing is applied, no `classification_ready` is
    /// pushed, no mail command is sent), and — for messages the user has deliberately filed (not
    /// the inbox) — its current placement is mined as implicit-positive filing evidence to warm the
    /// crystallization clusters. The mined proposals then surface in the Review queue through the
    /// ordinary `generate_proposals` pass. Returns per-page counts so the extension can render a
    /// resumable progress chip.
    async fn handle_triage_existing(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let parsed: TriageExistingMailPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        let record_placements = parsed.record_placements.unwrap_or(true);
        let mut classified = 0usize;
        let mut needs_review = 0usize;
        let mut placements_recorded = 0usize;

        for item in parsed.messages {
            let message = item.into_message_data();
            // Folder-history mining (§3.5): a message the user deliberately filed elsewhere is an
            // implicit positive for "this domain belongs in that folder". The inbox is skipped — an
            // un-triaged arrival is not a deliberate placement. Stamped `existing_placement` so the
            // back-test and metrics can tell observed placements from deliberate corrections (and so
            // it is never confused with a rule's own auto-applied move). Re-mining is idempotent: a
            // partial-unique index (migration 0007) rejects a second placement for the same message,
            // so a re-run's duplicate insert simply fails here and is not counted — no inflated
            // support for the same underlying mail.
            if record_placements {
                if let Some(row) = self.existing_placement_row(&message) {
                    if self
                        .learning
                        .record_feedback(TaskFeedback::Filing(row))
                        .await
                        .is_ok()
                    {
                        placements_recorded += 1;
                    }
                }
            }
            // Dry-run classify: produce the verdict WITHOUT applying or notifying — this never
            // mutates mail. A message that fails to classify is simply skipped from the counts.
            if let Ok(outcome) = self.planning.handle_new_mail(message).await {
                classified += 1;
                if outcome.classification.needs_review {
                    needs_review += 1;
                }
            }
        }

        self.send(ok_response(
            request_id,
            json!({
                "classified": classified,
                "needs_review": needs_review,
                "placements_recorded": placements_recorded,
            }),
        ))
    }

    /// Synthesize an implicit-positive filing row from a message's CURRENT placement, for
    /// folder-history mining. `None` when the message has no usable sender domain or sits in the
    /// inbox (not a deliberate placement, so never mined).
    fn existing_placement_row(
        &self,
        message: &mailmate_common::mail::MessageData,
    ) -> Option<FilingFeedbackRow> {
        let folder = message.folder_id.as_str();
        let is_inbox =
            folder.eq_ignore_ascii_case("inbox") || folder.to_ascii_lowercase().ends_with("/inbox");
        if is_inbox {
            return None;
        }
        let sender_domain = message
            .headers
            .from
            .rsplit_once('@')
            .map(|(_, d)| d.trim_end_matches('>').trim().to_ascii_lowercase())
            .filter(|d| !d.is_empty())?;
        Some(FilingFeedbackRow {
            id: FilingFeedback::fresh_id(),
            message_id: message.id.clone().unwrap_or_else(MessageId::fresh),
            pinned_versions: PinnedVersions::default(),
            sender_domain: Some(sender_domain),
            ai_suggested_folder: None,
            human_chosen_folder: message.folder_id.clone(),
            basis: Some("existing_placement".to_owned()),
            // An observed placement reinforces (positive), it is not a correction of the AI.
            polarity: FeedbackPolarity::Positive,
            matched_rule_id: None,
            created_at: self.clock.now(),
        })
    }

    /// Whether the global pause kill-switch is engaged (a wired admin surface holds the state).
    fn is_paused(&self) -> bool {
        self.admin
            .as_ref()
            .is_some_and(|a| a.config.lock().unwrap().paused)
    }

    /// The effective triage policy for a just-classified arrival: the MOST RESTRICTIVE of the
    /// global pause (paused ⇒ a floor of `Suggest`), the per-account scope (out of scope ⇒ `Off`),
    /// and the per-category policy across the message's labels (`Off` if any label's category is
    /// `Off`, else `Suggest` if any is `Suggest`, else `Auto`). A router without an admin surface
    /// (the bare Phase-10 shape) has no config to read, so it stays at the open default `Auto`.
    fn triage_policy_for(
        &self,
        account_id: &AccountId,
        classification: &Classification,
    ) -> CategoryPolicy {
        let Some(admin) = &self.admin else {
            return CategoryPolicy::Auto;
        };
        // Pause is a global floor of `Suggest`. Read it FIRST: `is_paused` takes (and releases)
        // the config lock, and `std::sync::Mutex` is not re-entrant — we must not still hold the
        // guard taken below.
        let mut policy = if self.is_paused() {
            CategoryPolicy::Suggest
        } else {
            CategoryPolicy::Auto
        };
        let config = admin.config.lock().unwrap();
        // An out-of-scope account is a hard silence, regardless of category.
        if !config.triage.account_in_scope(account_id.as_str()) {
            return CategoryPolicy::Off;
        }
        // Category keys are always lower-cased (starters + tag-mapped), but a classification label
        // is free-form and need not be — so match case-insensitively, else a policy on `work`
        // would silently miss a `Work` label and the category would not actually be silenced.
        for label in &classification.labels {
            let key = label.to_ascii_lowercase();
            policy = policy.most_restrictive(config.triage.policy_for(&key));
        }
        policy
    }

    /// Apply every policy-allowed action through the mail client, auditing each with its
    /// provenance (who authored it + how to reverse it), and return the applied records so the
    /// notification can list them with a real Undo affordance.
    ///
    /// **Apply gate.** `plan.allowed_actions` are translated from the rule engine's
    /// `applied_effects`, which come only from **active** rules — so an auto-applied action is
    /// active-rule-authored by construction. `authored_by` records that invariant on the wire
    /// and in the audit trail; the precise per-action `rule_id` (the planner threads it from
    /// `AppliedEffect::rule_id` through the guard's length-matched `allowed_authored_by` sidecar)
    /// is stamped onto the `action_applied` row's indexed `rule_id` column here — that is the
    /// per-rule **fires** denominator the Rules-manager hit-rate and the decay undo-*rate* read.
    async fn apply_allowed(
        &self,
        plan: &GuardedActionPlan,
        origin_folder: &FolderId,
    ) -> Vec<AppliedAction> {
        let mut applied = Vec::new();
        for (index, action) in plan.allowed_actions.iter().enumerate() {
            // The authoring rule for this allowed action, read position-wise from the guard's
            // length-matched sidecar (degrading a missing/short entry to `None` rather than
            // panicking — a plan built without provenance simply has none).
            let rule_id = plan.allowed_authored_by.get(index).cloned().flatten();
            let result = match action {
                PlannedAction::CreateDraft { draft } => self
                    .mail_client
                    .create_draft(draft.clone())
                    .await
                    .map(|_| ()),
                other => match planned_to_mail_action(other) {
                    Some(mail_action) => self.mail_client.apply(mail_action).await,
                    // RequireReview is a surfaced flag, never auto-applied.
                    None => continue,
                },
            };
            match result {
                Ok(()) => {
                    let reverses_to = reverse_of(action, origin_folder);
                    // Audit the action annotated with its provenance (kept at top level so the
                    // existing action fields stay readable), so the trail records who authored
                    // it and how it can be reversed.
                    let mut payload = serde_json::to_value(action).unwrap_or(Value::Null);
                    if let Value::Object(map) = &mut payload {
                        map.insert("authored_by".to_owned(), json!("active_rule"));
                        map.insert("reverses_to".to_owned(), json!(reverses_to));
                        if let Some(rule_id) = &rule_id {
                            map.insert("rule_id".to_owned(), json!(rule_id.as_str()));
                        }
                    }
                    let mut entry = AuditEntry::new(event_type::ACTION_APPLIED, Actor::System)
                        .with_payload(payload);
                    // Stamp the indexed `rule_id` column so `AuditQuery::rule_id` counts this rule's
                    // fires — the denominator the decay pass divides undos by. Action rules author
                    // auto-applied actions, so the kind is `Action`.
                    if let Some(rule_id) = &rule_id {
                        entry = entry.with_rule(RuleKind::Action, rule_id.clone());
                    }
                    let _ = self.audit.append(entry).await;
                    applied.push(AppliedAction {
                        action: action.clone(),
                        authored_by: "active_rule",
                        rule_id,
                        reverses_to,
                    });
                }
                Err(e) => {
                    let entry = AuditEntry::new("action_apply_failed", Actor::System)
                        .with_payload(json!({ "action": action, "error": e.to_string() }));
                    let _ = self.audit.append(entry).await;
                }
            }
        }
        applied
    }

    /// Generate an advisory, review-required reply draft.
    async fn handle_draft(&self, request_id: String, payload: Value) -> Result<(), TransportError> {
        let parsed: DraftReplyPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        self.draft_and_respond(request_id, parsed.into_request())
            .await
    }

    /// Re-draft a reply, folding the user's steer (quick-steer chips and/or free-text Adjust)
    /// into the guidance. It shares the whole draft path with [`Self::handle_draft`], so the
    /// response — body, commitments guard, rationale — is shaped identically; only the model
    /// guidance differs, and a fresh `draft_id` is assigned.
    async fn handle_regenerate(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let parsed: RegenerateDraftPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        self.draft_and_respond(request_id, parsed.into_request())
            .await
    }

    /// The shared draft path behind `draft_reply` and `regenerate_draft`: draft over the
    /// provider, run the **model-free** commitments guard over the body, and ship the
    /// review-required draft together with its guard report and rationale. The guard always
    /// runs on the host — it never depends on the model's cooperation — so the trust surface
    /// is present even when the model returns no safety notes.
    async fn draft_and_respond(
        &self,
        request_id: String,
        request: ReplyDraftRequest,
    ) -> Result<(), TransportError> {
        match self.draft.draft_reply(request).await {
            Ok(draft) => {
                let guard = mailmate_guard::scan_commitments(&draft.body);
                let commitments = serde_json::to_value(&guard).unwrap_or(Value::Null);
                self.send(ok_response(
                    request_id,
                    json!({
                        "draft_id": draft.draft_id,
                        "subject": draft.subject,
                        "body": draft.body,
                        "safety_notes": draft.safety_notes,
                        "rationale": draft.rationale,
                        "commitments": commitments,
                        "requires_human_review": draft.requires_human_review,
                    }),
                ))
            }
            Err(e) => self.send(error_response(
                request_id,
                "draft_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Enroll a tracked quote/proposal: create a `pipeline_item` and arm a workflow on it.
    async fn handle_enroll(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(followups) = &self.followups else {
            return self.send(followups_not_configured(request_id));
        };
        let parsed: EnrollPipelineItemPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        let item_id = match followups
            .pipeline_items
            .insert(parsed.into_new_item())
            .await
        {
            Ok(id) => id,
            Err(e) => {
                return self.send(error_response(
                    request_id,
                    "enroll_failed",
                    e.to_string(),
                    None,
                ))
            }
        };
        match followups
            .workflow_engine
            .arm(item_id.clone(), parsed.workflow_def_id())
            .await
        {
            Ok(instance_id) => {
                let _ = self
                    .audit
                    .append(
                        AuditEntry::new("pipeline_item_enrolled", Actor::User).with_payload(
                            json!({
                                "pipeline_item_id": item_id.as_str(),
                                "workflow_instance_id": instance_id.as_str(),
                                "workflow_id": parsed.workflow_id,
                            }),
                        ),
                    )
                    .await;
                self.send(ok_response(
                    request_id,
                    json!({
                        "pipeline_item_id": item_id,
                        "workflow_instance_id": instance_id,
                    }),
                ))
            }
            Err(e) => self.send(error_response(
                request_id,
                "enroll_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Mark a deal won/lost (exits the sequence and closes the stage).
    async fn handle_update_stage(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(followups) = &self.followups else {
            return self.send(followups_not_configured(request_id));
        };
        let parsed: UpdatePipelineStagePayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        let Some(event) = parsed.exit_event() else {
            return self.send(error_response(
                request_id,
                "invalid_stage",
                format!(
                    "update_pipeline_stage expects won/lost, got {:?}",
                    parsed.stage
                ),
                None,
            ));
        };
        self.exit(request_id, followups, parsed.item_id(), event)
            .await
    }

    /// Cancel a sequence on a pipeline item.
    async fn handle_cancel(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(followups) = &self.followups else {
            return self.send(followups_not_configured(request_id));
        };
        let parsed: CancelSequencePayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        self.exit(request_id, followups, parsed.item_id(), ExitEvent::Cancel)
            .await
    }

    /// Drive an exit event and answer with the instances it exited.
    async fn exit(
        &self,
        request_id: String,
        followups: &FollowUpSuite,
        item: mailmate_common::ids::PipelineItemId,
        event: ExitEvent,
    ) -> Result<(), TransportError> {
        let item_id = item.clone();
        match followups.exit_detector.on_exit_event(item, event).await {
            Ok(exited) => {
                self.audit_exit(&item_id, event, &exited).await;
                self.send(ok_response(
                    request_id,
                    json!({ "exited": exited, "event": event.as_str() }),
                ))
            }
            Err(e) => self.send(error_response(
                request_id,
                "exit_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Audit a workflow exit (the mutable-state instance's transition is reconstructable from
    /// the audit timeline — see the data-model's mutable-state exception). Actor by origin: a
    /// reply is the system observing inbound mail; won/lost/cancel are user decisions.
    async fn audit_exit(
        &self,
        item: &mailmate_common::ids::PipelineItemId,
        event: ExitEvent,
        exited: &[mailmate_common::ids::WorkflowInstanceId],
    ) {
        let actor = match event {
            // System-detected exits (a reply landed, or an NDR bounced back).
            ExitEvent::ReplyReceived | ExitEvent::Bounced => Actor::System,
            ExitEvent::Won | ExitEvent::Lost | ExitEvent::Cancel => Actor::User,
        };
        let _ = self
            .audit
            .append(
                AuditEntry::new("workflow_exited", actor).with_payload(json!({
                    "pipeline_item_id": item.as_str(),
                    "event": event.as_str(),
                    "exited_instances": exited.iter().map(|i| i.as_str()).collect::<Vec<_>>(),
                })),
            )
            .await;
    }

    /// Push a follow-up's next step out (`reschedule_followup` / `snooze`).
    async fn handle_reschedule(
        &self,
        request_id: String,
        payload: Value,
        snooze: bool,
    ) -> Result<(), TransportError> {
        let Some(followups) = &self.followups else {
            return self.send(followups_not_configured(request_id));
        };
        let parsed: RescheduleFollowupPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        match followups
            .workflow_engine
            .reschedule(parsed.instance_id(), parsed.next_due_at, snooze)
            .await
        {
            Ok(()) => self.send(ok_response(
                request_id,
                json!({ "rescheduled": true, "snoozed": snooze }),
            )),
            Err(e) => self.send(error_response(
                request_id,
                "reschedule_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Resolve a surfaced follow-up draft (`review_followup`): advance the cursor. No
    /// resolution sends — `send` means the human already dispatched the draft.
    async fn handle_review_followup(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(followups) = &self.followups else {
            return self.send(followups_not_configured(request_id));
        };
        let parsed: ReviewFollowupPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        let Some(resolution) = parsed.resolution() else {
            return self.send(error_response(
                request_id,
                "invalid_resolution",
                format!(
                    "review_followup expects send/edit/skip, got {:?}",
                    parsed.resolution
                ),
                None,
            ));
        };
        match followups
            .workflow_engine
            .resolve_review(parsed.instance_id(), resolution, self.clock.now())
            .await
        {
            Ok(()) => self.send(ok_response(
                request_id,
                json!({ "resolved": true, "resolution": resolution.as_str() }),
            )),
            Err(e) => self.send(error_response(
                request_id,
                "review_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Explain a decision: return the audit timeline for one message (the classification,
    /// the applied/blocked actions, the corrections — the data any review/explanation UI
    /// renders). Uses the always-present audit store, so it needs no admin wiring.
    async fn handle_explain(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let parsed: ExplainDecisionPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        let Some(message_id) = parsed.message_id() else {
            return self.send(error_response(
                request_id,
                "invalid_payload",
                "explain_decision requires message_id or thunderbird_message_id",
                None,
            ));
        };
        let query = AuditQuery {
            message_id: Some(message_id.clone()),
            limit: parsed.limit,
            ..AuditQuery::default()
        };
        match self.audit.query(query).await {
            Ok(entries) => {
                let timeline: Vec<Value> = entries
                    .iter()
                    .map(|entry| {
                        json!({
                            "id": entry.id,
                            "event_type": entry.event_type,
                            "actor": entry.actor.as_str(),
                            "created_at": entry.created_at,
                            "payload": entry.payload,
                        })
                    })
                    .collect();
                self.send(ok_response(
                    request_id,
                    json!({ "message_id": message_id, "timeline": timeline }),
                ))
            }
            Err(e) => self.send(error_response(
                request_id,
                "explain_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Power the dashboard Activity tab's cross-message **global** stream. `explain_decision` is
    /// per-message (it requires and keys on one `message_id`); this drops that constraint and
    /// reads the newest audit entries, optionally narrowed to one of the six event-type
    /// *families* the UI's filter chips expose. Family matching happens here because the audit
    /// store keys on a single exact `event_type` (one family spans several event types), so we
    /// over-read when a filter is present and cap to `limit` after grouping. Read-only, served
    /// from the always-present audit store — no admin wiring required.
    async fn handle_list_recent_activity(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        // A missing/empty payload is a valid "everything, default limit" request.
        let parsed: ListRecentActivityPayload = serde_json::from_value(payload).unwrap_or_default();
        let limit = parsed.limit.unwrap_or(50).min(500);
        let family = parsed.event_type_filter.as_deref().filter(|f| *f != "all");
        // The store filters by exact event_type only, so when narrowing to a multi-type family
        // we over-read and group here. Bound the over-read so a pathological filter can't scan
        // the whole log.
        let fetch = if family.is_some() {
            limit.saturating_mul(8).min(2000)
        } else {
            limit
        };
        let query = AuditQuery {
            limit: Some(fetch),
            ..AuditQuery::default()
        };
        match self.audit.query(query).await {
            Ok(entries) => {
                let events: Vec<Value> = entries
                    .iter()
                    .filter(|e| activity_family_matches(family, &e.event_type))
                    .take(limit)
                    .map(|e| {
                        json!({
                            "id": e.id,
                            "event_type": e.event_type,
                            "actor": e.actor.as_str(),
                            "message_id": e.message_id,
                            "rule_id": e.rule_id,
                            "proposal_id": e.proposal_id,
                            "created_at": e.created_at,
                            "payload": e.payload,
                        })
                    })
                    .collect();
                self.send(ok_response(request_id, json!({ "events": events })))
            }
            Err(e) => self.send(error_response(
                request_id,
                "list_activity_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Render the Follow-ups pipeline on dashboard open: each tracked deal (pipeline item) joined
    /// to its workflow instance's status + next-due step. Read-only; needs the follow-up suite
    /// (the instance store lives there, outside the core's [`Ports`]).
    async fn handle_list_followups(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(followups) = &self.followups else {
            return self.send(followups_not_configured(request_id));
        };
        let parsed: ListFollowupsPayload = serde_json::from_value(payload).unwrap_or_default();
        let limit = parsed.limit.unwrap_or(100).min(500);
        let filter = parsed.status_filter.as_deref().filter(|f| *f != "all");

        let items = match followups
            .pipeline_items
            .query(PipelineItemQuery {
                account_id: None,
                stage: None,
                limit: Some(limit),
            })
            .await
        {
            Ok(items) => items,
            Err(e) => {
                return self.send(error_response(
                    request_id,
                    "list_followups_failed",
                    e.to_string(),
                    None,
                ))
            }
        };

        let mut out = Vec::new();
        for item in &items {
            // Join the deal to its most relevant instance: a live (non-terminal) one if present,
            // else the latest. A deal with no instance is still a tracked deal (just unarmed).
            let instances = followups
                .instances
                .list_by_pipeline_item(&item.id)
                .await
                .unwrap_or_default();
            let inst = instances
                .iter()
                .find(|i| !i.status.is_terminal())
                .or_else(|| instances.last());
            let needs_attention = inst.is_some_and(|i| {
                matches!(
                    i.status,
                    WorkflowInstanceStatus::AwaitingReview | WorkflowInstanceStatus::NeedsAttention
                )
            });

            if !followup_status_matches(filter, item.stage, inst.map(|i| i.status), needs_attention)
            {
                continue;
            }

            // The pipeline anchor is an internal id (`msg_tb_<tb>`); recover the Thunderbird id for
            // the dashboard deep-link.
            let anchor_tb = item.anchor_message_id.as_ref().map(|m| {
                m.as_str()
                    .strip_prefix("msg_tb_")
                    .unwrap_or(m.as_str())
                    .to_owned()
            });

            out.push(json!({
                "pipeline_item_id": item.id,
                "title": item.title,
                "thread_id": item.thread_id,
                "stage": item.stage.as_str(),
                "anchor_thunderbird_message_id": anchor_tb,
                "workflow_instance_id": inst.map(|i| &i.id),
                "status": inst.map(|i| i.status.as_str()),
                "next_due_at": inst.and_then(|i| i.next_due_at),
                "current_step_index": inst.map(|i| i.current_step_index),
                "needs_attention": needs_attention,
            }));
        }
        self.send(ok_response(request_id, json!({ "followups": out })))
    }

    /// List the agent proposals awaiting human review — the review UI's work queue.
    async fn handle_list_reviews(&self, request_id: String) -> Result<(), TransportError> {
        let Some(admin) = &self.admin else {
            return self.send(admin_not_configured(request_id));
        };
        match admin
            .proposals
            .list_by_status(ProposalStatus::PendingReview)
            .await
        {
            Ok(proposals) => {
                let pending: Vec<Value> = proposals
                    .iter()
                    .map(|p| {
                        json!({
                            "id": p.id,
                            "title": p.title,
                            "proposal_type": p.proposal_type.as_str(),
                            "recommended_status": p.recommended_status.as_str(),
                            "risk_level": p.risk_level.as_str(),
                            "rationale": p.rationale,
                            // The candidate rule's condition→effect AST (the card renders it in
                            // English) and the crystallization back-test the gate admitted it on,
                            // so a low-risk proposal can be approved from the card without drilling
                            // into the detail view. `null` for proposals that carry neither.
                            "rule_draft": p.rule_draft,
                            "back_test": p.back_test,
                            // For a proposal that references an existing rule (a retire), the
                            // card needs its target to name it and to act on acceptance. `null`
                            // for a `new_rule` proposal, which carries a draft instead.
                            "target_rule_id": p.target_rule_id,
                            "target_rule_kind": p.target_rule_kind,
                            // Conflicts with existing active rules (subsumption/overlap/contradiction).
                            // A non-empty list is why a proposal recommends pending_human_review; the
                            // card warns the user and lists each conflict's description.
                            "conflicts": conflicts_json(&p.conflicts),
                        })
                    })
                    .collect();
                self.send(ok_response(
                    request_id,
                    json!({ "pending_reviews": pending }),
                ))
            }
            Err(e) => self.send(error_response(
                request_id,
                "list_reviews_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// List every evaluated (active + shadow) rule for the Rules manager: each rule's lifecycle,
    /// its condition→effect AST (the tab renders it in English), and its **correction count** —
    /// how many times a human undid one of this rule's auto-applied actions (the backed quality
    /// signal). The precise per-firing hit-rate awaits the per-apply rule provenance (a tracked
    /// follow-up); a `null` undo bucket here means "not yet tracked", never a confident zero.
    async fn handle_list_rules(&self, request_id: String) -> Result<(), TransportError> {
        let Some(reload) = &self.rule_reload else {
            // No composition root wired (the bare router): no rule store to read.
            return self.send(ok_response(request_id, json!({ "rules": [] })));
        };
        let rules_port = reload.rules.as_ref();
        let mut rules =
            match crate::runtime::rule_manager_snapshot(rules_port, RuleKind::Classification).await
            {
                Ok(r) => r,
                Err(e) => {
                    return self.send(error_response(
                        request_id,
                        "list_rules_failed",
                        e.to_string(),
                        None,
                    ))
                }
            };
        match crate::runtime::rule_manager_snapshot(rules_port, RuleKind::Action).await {
            Ok(action) => rules.extend(action),
            Err(e) => {
                return self.send(error_response(
                    request_id,
                    "list_rules_failed",
                    e.to_string(),
                    None,
                ))
            }
        }

        let mut cards = Vec::with_capacity(rules.len());
        for r in &rules {
            // The per-rule correction signal: undos of this rule's auto-applied actions, by the
            // now-stamped audit `rule_id` column. A store-read failure degrades to `null` (unknown),
            // never a misleading 0.
            let undo_count = match self
                .audit
                .query(AuditQuery {
                    event_type: Some(event_type::ACTION_UNDONE.to_owned()),
                    rule_id: Some(r.rule_id.clone()),
                    message_id: None,
                    limit: None,
                })
                .await
            {
                Ok(rows) => json!(rows.len()),
                Err(_) => Value::Null,
            };
            cards.push(json!({
                "rule_id": r.rule_id,
                "kind": r.kind.as_str(),
                "scope": r.scope.as_str(),
                "band": r.band.as_str(),
                "status": r.status.as_str(),
                "risk_level": r.version.risk_level.as_str(),
                "version_number": r.version.version_number,
                "condition": r.version.condition,
                "effect": r.version.effect,
                "undo_count": undo_count,
            }));
        }
        self.send(ok_response(request_id, json!({ "rules": cards })))
    }

    /// Set a rule's lifecycle status (the Rules-manager enable/disable, and `shadow → active`
    /// promotion). Validates the status token, updates the store, then **hot-reloads** the engines
    /// in place so the change takes effect immediately (no host restart) — the same reload the
    /// proposal-activation path uses. Only the evaluated/parked statuses a human can pick are
    /// accepted here; the lifecycle-internal ones (`draft`/`pending_human_review`/`retired`) are
    /// not user-settable from this surface.
    async fn handle_set_rule_status(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(reload) = &self.rule_reload else {
            return self.send(error_response(
                request_id,
                "rules_not_configured",
                "the rule store is not wired in this build",
                None,
            ));
        };
        let Some(rule_id) = payload
            .get("rule_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return self.send(invalid_payload_msg(
                request_id,
                "set_rule_status requires rule_id",
            ));
        };
        let Some(kind) = payload
            .get("kind")
            .and_then(Value::as_str)
            .and_then(RuleKind::from_db_str)
        else {
            return self.send(invalid_payload_msg(
                request_id,
                "set_rule_status requires kind ∈ classification|action",
            ));
        };
        let Some(status) = payload
            .get("status")
            .and_then(Value::as_str)
            .and_then(RuleStatus::from_db_str)
        else {
            return self.send(invalid_payload_msg(
                request_id,
                "set_rule_status: unknown status",
            ));
        };
        // Only the user-settable target statuses: enable (active), pause to shadow, or disable. The
        // lifecycle-internal statuses are not reachable from the manager (they would skip review).
        if !matches!(
            status,
            RuleStatus::Active | RuleStatus::ShadowMode | RuleStatus::Disabled
        ) {
            return self.send(invalid_payload_msg(
                request_id,
                "set_rule_status accepts only active | shadow_mode | disabled",
            ));
        }
        // **Review-gate guard.** This surface only re-flips a rule that is ALREADY past review —
        // i.e. currently active/shadow/disabled (exactly what the manager lists). It must never
        // promote a `draft`/`pending_human_review`/`rejected`/`retired` rule into `active`, which
        // would bypass the proposal-review materialization gate. We confirm the CURRENT status by
        // checking the rule is in the manager snapshot; if it is not, refuse.
        let manageable =
            match crate::runtime::rule_manager_snapshot(reload.rules.as_ref(), kind).await {
                Ok(snapshot) => snapshot.iter().any(|r| r.rule_id.as_str() == rule_id),
                Err(e) => {
                    return self.send(error_response(
                        request_id,
                        "set_rule_status_failed",
                        e.to_string(),
                        None,
                    ))
                }
            };
        if !manageable {
            return self.send(error_response(
                request_id,
                "rule_not_manageable",
                "that rule is not in a manageable state — an unreviewed rule must be approved through review, not activated here",
                None,
            ));
        }
        if let Err(e) = reload
            .rules
            .update_rule_status(&RuleId::from(rule_id), kind, status)
            .await
        {
            return self.send(error_response(
                request_id,
                "set_rule_status_failed",
                e.to_string(),
                None,
            ));
        }
        // Enabling a rule here is a real activation — stamp `rule_activated` so the audit timeline
        // has an activation reference for it. Without this, a rule promoted to Active from the Rules
        // manager (rather than via proposal acceptance) has no activation timestamp, so the decay
        // pass could never judge it stale for "never fired since it went active". Mirrors the
        // activation audit the review handler writes on accept-active.
        if status == RuleStatus::Active {
            let entry = AuditEntry::new(event_type::RULE_ACTIVATED, Actor::User)
                .with_rule(kind, RuleId::from(rule_id))
                .with_payload(
                    json!({ "to": RuleStatus::Active.as_str(), "source": "rules_manager" }),
                );
            if let Err(e) = self.audit.append(entry).await {
                log::warn!(
                    "rule_activated audit write failed (staleness reference may be missing): {e}"
                );
            }
        }
        // Hot-reload so the new status is live at once (an enabled rule fires now; a disabled one
        // stops firing now) — never on the next restart.
        if let Err(e) = self.reload_rules().await {
            return self.send(error_response(
                request_id,
                "rule_reload_failed",
                e.to_string(),
                None,
            ));
        }
        self.send(ok_response(
            request_id,
            json!({ "rule_id": rule_id, "kind": kind.as_str(), "status": status.as_str(), "reloaded": true }),
        ))
    }

    /// Run an on-device Tier-2 training pass (Phase 8): train a Burn classifier from the user's
    /// classification corrections, evaluate the **reloaded** artifact on a held-out split, and —
    /// only if held-out precision clears the gate — promote it and hot-swap it into the live
    /// cascade. The `tier2_trained` audit row records the metrics + the activation verdict; the
    /// response reports the same so the UI can show "trained, precision X, activated Y". Degrades
    /// honestly: a build without the service answers not-configured, and a run with too little
    /// data reports `activated:false` rather than fabricating a model.
    async fn handle_train_tier2(&self, request_id: String) -> Result<(), TransportError> {
        let Some(service) = &self.tier2_training else {
            return self.send(error_response(
                request_id,
                "tier2_training_not_configured",
                "on-device Tier-2 training is not wired in this build",
                None,
            ));
        };
        let report = match service.run().await {
            Ok(report) => report,
            Err(e) => {
                return self.send(error_response(
                    request_id,
                    "tier2_training_failed",
                    e.to_string(),
                    None,
                ))
            }
        };
        let metrics = json!({
            "activated": report.activated,
            "precision": report.eval.precision,
            "recall": report.eval.recall,
            "accuracy": report.eval.accuracy,
            "eval_n": report.eval.n,
            "train_count": report.train_count,
            "precision_gate": report.precision_gate,
        });
        // The auditable record that the gate flipped active only above threshold.
        let entry =
            AuditEntry::new(event_type::TIER2_TRAINED, Actor::System).with_payload(metrics.clone());
        if let Err(e) = self.audit.append(entry).await {
            log::warn!("tier2_trained audit write failed: {e}");
        }
        let mut response = metrics;
        response["artifact_path"] = json!(report.artifact_path);
        response["calibration_version"] = json!(crate::tier2_training::TIER2_CALIBRATION_VERSION);
        self.send(ok_response(request_id, response))
    }

    /// Erase one message and everything derived from it (Phase 9 delete-my-data). The per-message
    /// audit rows go with it inside the transaction; the `data_forgotten` tombstone appended after
    /// keeps the *act* accountable without resurrecting the erased content.
    async fn handle_forget_message(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(data_rights) = &self.data_rights else {
            return self.send(data_rights_not_configured(request_id));
        };
        let Some(message_id) = payload.get("message_id").and_then(Value::as_str) else {
            return self.send(error_response(
                request_id,
                "invalid_payload",
                "forget_message requires a string `message_id`",
                None,
            ));
        };
        let id = MessageId::from(message_id);
        let report = match data_rights.forget_message(&id).await {
            Ok(r) => r,
            Err(e) => return self.send(erasure_failed(request_id, &e)),
        };
        let entry = AuditEntry::new(event_type::DATA_FORGOTTEN, Actor::User)
            .with_message(id)
            .with_payload(json!({ "scope": "message", "removed": report.removed }));
        if let Err(e) = self.audit.append(entry).await {
            log::warn!("data_forgotten audit write failed: {e}");
        }
        self.send(ok_response(
            request_id,
            json!({ "scope": "message", "removed": report.removed, "total": report.total() }),
        ))
    }

    /// Erase every message from one sender address and that sender's learned profile.
    async fn handle_forget_sender(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(data_rights) = &self.data_rights else {
            return self.send(data_rights_not_configured(request_id));
        };
        let Some(sender) = payload.get("sender_email").and_then(Value::as_str) else {
            return self.send(error_response(
                request_id,
                "invalid_payload",
                "forget_sender requires a string `sender_email`",
                None,
            ));
        };
        let report = match data_rights.forget_sender(sender).await {
            Ok(r) => r,
            Err(e) => return self.send(erasure_failed(request_id, &e)),
        };
        let entry = AuditEntry::new(event_type::DATA_FORGOTTEN, Actor::User).with_payload(
            json!({ "scope": "sender", "sender_email": sender, "removed": report.removed }),
        );
        if let Err(e) = self.audit.append(entry).await {
            log::warn!("data_forgotten audit write failed: {e}");
        }
        self.send(ok_response(
            request_id,
            json!({ "scope": "sender", "removed": report.removed, "total": report.total() }),
        ))
    }

    /// Reset all learning: delete the correction corpus, the learned/shadow rules, and the
    /// learned sender profiles. Built-in safety / human-hard / fallback rules and the stored
    /// messages themselves are kept.
    async fn handle_reset_learning(&self, request_id: String) -> Result<(), TransportError> {
        let Some(data_rights) = &self.data_rights else {
            return self.send(data_rights_not_configured(request_id));
        };
        let report = match data_rights.reset_learning().await {
            Ok(r) => r,
            Err(e) => return self.send(erasure_failed(request_id, &e)),
        };
        let entry = AuditEntry::new(event_type::DATA_FORGOTTEN, Actor::User)
            .with_payload(json!({ "scope": "learning", "removed": report.removed }));
        if let Err(e) = self.audit.append(entry).await {
            log::warn!("data_forgotten audit write failed: {e}");
        }
        self.send(ok_response(
            request_id,
            json!({ "scope": "learning", "removed": report.removed, "total": report.total() }),
        ))
    }

    /// Export everything stored about the user (Phase 9 portability). The audit row carries only
    /// counts — never the exported content — so the log is not itself a copy of the data.
    async fn handle_export_my_data(&self, request_id: String) -> Result<(), TransportError> {
        let Some(data_rights) = &self.data_rights else {
            return self.send(data_rights_not_configured(request_id));
        };
        let export = match data_rights.export().await {
            Ok(e) => e,
            Err(e) => return self.send(erasure_failed(request_id, &e)),
        };
        let counts = json!({
            "messages": export.messages.len(),
            "classification_feedback": export.classification_feedback.len(),
            "rules": export.rules.len(),
        });
        let entry = AuditEntry::new(event_type::DATA_EXPORTED, Actor::User)
            .with_payload(json!({ "counts": counts }));
        if let Err(e) = self.audit.append(entry).await {
            log::warn!("data_exported audit write failed: {e}");
        }
        match serde_json::to_value(&export) {
            Ok(value) => self.send(ok_response(request_id, value)),
            Err(e) => self.send(error_response(
                request_id,
                "export_serialize_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Arm a durable notify-only reminder over a message/thread (Phase 9 snooze / remind-me /
    /// send-later). `due_at` is an RFC-3339 instant; `title` is required, the rest optional.
    async fn handle_remind_me(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(reminders) = &self.reminders else {
            return self.send(reminders_not_configured(request_id));
        };
        let Some(title) = payload.get("title").and_then(Value::as_str) else {
            return self.send(error_response(
                request_id,
                "invalid_payload",
                "remind_me requires a string `title`",
                None,
            ));
        };
        let Some(due_at) = payload
            .get("due_at")
            .and_then(Value::as_str)
            .and_then(|s| Timestamp::parse_rfc3339(s).ok())
        else {
            return self.send(error_response(
                request_id,
                "invalid_payload",
                "remind_me requires an RFC-3339 `due_at`",
                None,
            ));
        };
        let str_field = |k: &str| payload.get(k).and_then(Value::as_str).map(str::to_owned);
        let new = NewReminder {
            id: ReminderId::fresh(),
            message_id: str_field("message_id").map(MessageId::from),
            thread_id: str_field("thread_id").map(ThreadId::from),
            account_id: str_field("account_id").map(AccountId::from),
            title: title.to_owned(),
            note: str_field("note"),
            due_at,
            created_at: self.clock.now(),
        };
        let due_iso = due_at.to_rfc3339();
        match reminders.create(new).await {
            Ok(id) => self.send(ok_response(
                request_id,
                json!({ "reminder_id": id.as_str(), "due_at": due_iso }),
            )),
            Err(e) => self.send(erasure_failed(request_id, &e)),
        }
    }

    /// Snooze (reschedule) an existing reminder to a new `due_at`, re-arming it if it had fired.
    async fn handle_snooze_reminder(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(reminders) = &self.reminders else {
            return self.send(reminders_not_configured(request_id));
        };
        let (Some(id), Some(due_at)) = (
            payload.get("reminder_id").and_then(Value::as_str),
            payload
                .get("due_at")
                .and_then(Value::as_str)
                .and_then(|s| Timestamp::parse_rfc3339(s).ok()),
        ) else {
            return self.send(error_response(
                request_id,
                "invalid_payload",
                "snooze_reminder requires `reminder_id` and an RFC-3339 `due_at`",
                None,
            ));
        };
        match reminders.reschedule(&ReminderId::from(id), due_at).await {
            Ok(()) => self.send(ok_response(
                request_id,
                json!({ "rescheduled": true, "due_at": due_at.to_rfc3339() }),
            )),
            Err(e) => self.send(erasure_failed(request_id, &e)),
        }
    }

    /// Cancel (dismiss) a reminder before it fires.
    async fn handle_cancel_reminder(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(reminders) = &self.reminders else {
            return self.send(reminders_not_configured(request_id));
        };
        let Some(id) = payload.get("reminder_id").and_then(Value::as_str) else {
            return self.send(error_response(
                request_id,
                "invalid_payload",
                "cancel_reminder requires a string `reminder_id`",
                None,
            ));
        };
        match reminders.cancel(&ReminderId::from(id)).await {
            Ok(()) => self.send(ok_response(request_id, json!({ "cancelled": true }))),
            Err(e) => self.send(erasure_failed(request_id, &e)),
        }
    }

    /// List the still-pending reminders (soonest-due first) for the UI.
    async fn handle_list_reminders(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(reminders) = &self.reminders else {
            return self.send(reminders_not_configured(request_id));
        };
        let limit = payload
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(100, |n| n as usize);
        match reminders.list_pending(limit).await {
            Ok(list) => {
                let items: Vec<Value> = list.iter().map(reminder_summary).collect();
                self.send(ok_response(request_id, json!({ "reminders": items })))
            }
            Err(e) => self.send(erasure_failed(request_id, &e)),
        }
    }

    /// The notify-only reminder drain: surface a `reminder_due` notification for each reminder
    /// that has come due (**at most one batch cap** per call, so a periodic tick can't storm), then
    /// mark it fired so it never re-nudges. Returns how many reminders were nudged this pass — the
    /// launch catch-up loops on this (see [`drain_reminders_to_empty`](Self::drain_reminders_to_empty))
    /// so a long-offline backlog still fully drains even with no periodic tick configured.
    /// Host-initiated (not a request). The notification is emitted **before** `mark_fired`, so a
    /// transport failure leaves the reminder pending to retry — a missed nudge, never a silent loss.
    ///
    /// # Errors
    /// Propagates a [`TransportError`] from emitting a frame. A storage failure is audited and
    /// ends the sweep without erroring the channel.
    pub async fn drain_reminders(&self) -> Result<usize, TransportError> {
        let Some(reminders) = &self.reminders else {
            return Ok(0);
        };
        let now = self.clock.now();
        let due = match reminders.list_due(now, self.reminder_batch_cap).await {
            Ok(d) => d,
            Err(e) => {
                let _ = self
                    .audit
                    .append(
                        AuditEntry::new("reminder_drain_failed", Actor::System)
                            .with_payload(json!({ "error": e.to_string() })),
                    )
                    .await;
                return Ok(0);
            }
        };
        let nudged = due.len();
        for reminder in &due {
            self.send(Frame::Notification {
                protocol_version: ProtocolVersion::default(),
                notification_id: format!("ntf_reminder_{}", reminder.id),
                type_: "reminder_due".to_owned(),
                payload: reminder_summary(reminder),
            })?;
            // Only mark fired AFTER the nudge is on the wire (a failed send above returns early,
            // leaving the reminder pending to retry next drain).
            if let Err(e) = reminders.mark_fired(&reminder.id, now).await {
                log::warn!(
                    "reminder {} fired-notification sent but mark_fired failed: {e}",
                    reminder.id
                );
                continue;
            }
            let mut entry = AuditEntry::new(event_type::REMINDER_FIRED, Actor::System)
                .with_payload(
                    json!({ "reminder_id": reminder.id.as_str(), "title": reminder.title }),
                );
            if let Some(mid) = &reminder.message_id {
                entry = entry.with_message(mid.clone());
            }
            if let Err(e) = self.audit.append(entry).await {
                log::warn!("reminder_fired audit write failed: {e}");
            }
        }
        Ok(nudged)
    }

    /// Drain reminders repeatedly until a pass nudges nothing — the launch catch-up, so a backlog
    /// that accumulated while the host was offline fully clears even when no periodic tick is
    /// configured (`tick_seconds = 0`). Each pass is still batch-capped (bounded memory per query);
    /// a hard iteration ceiling backstops any pathological non-converging loop.
    ///
    /// # Errors
    /// Propagates a [`TransportError`] from emitting a frame.
    pub async fn drain_reminders_to_empty(&self) -> Result<(), TransportError> {
        for _ in 0..MAX_CATCH_UP_PASSES {
            if self.drain_reminders().await? == 0 {
                break;
            }
        }
        Ok(())
    }

    /// Apply a human's accept/reject decision to a pending agent proposal — the Proposals-tab
    /// materialization gate. Acceptance is the **only** path that creates the recommended rule,
    /// always materializing into the recommended status (shadow / pending-review) first.
    /// `accept_active` then performs the *separate, explicit* activation that turns the rule live
    /// (a distinct `rule_activated` transition inside the review port); `accept_for_shadow_mode`
    /// leaves it shadow. A rejection records the curator's negative signal. The response reports
    /// both the proposal disposition (`resulting_status`) and the resulting rule mode
    /// (`rule_status` — `shadow_mode` / `active`). Served through the always-present
    /// proposal-review port, so it needs no admin wiring.
    async fn handle_review_proposal(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let parsed: ReviewRuleProposalPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        let proposal_id = ProposalId::from(parsed.proposal_id.as_str());
        let decision = match parsed.decision.as_str() {
            "accept_for_shadow_mode" => ReviewDecision::accept(proposal_id),
            // The explicit "approve & activate" path: materialize, then activate the rule live.
            "accept_active" => ReviewDecision::accept_and_activate(proposal_id),
            "reject" => ReviewDecision::reject(
                proposal_id,
                parsed.reason_code.unwrap_or_else(|| "rejected".to_owned()),
            ),
            other => {
                return self.send(error_response(
                    request_id,
                    "invalid_decision",
                    format!(
                        "decision must be accept_for_shadow_mode | accept_active | reject, got {other:?}"
                    ),
                    None,
                ));
            }
        };
        match self.proposal_review.review(decision).await {
            Ok(outcome) => {
                // ANY accepted proposal can change the engines' active/shadow snapshot — not just a
                // newly-created rule. A retire flips an ACTIVE rule to Retired and a refine appends a
                // new version, both returning no `created_rule_id`; gating the reload on rule
                // creation left the retired/refined rule firing from the stale in-memory snapshot
                // until a restart (it kept auto-applying the very action the user retired it for).
                // Reload on acceptance generally; a rejection mutates nothing, so it stays a no-op.
                if outcome.new_status == ProposalStatus::Accepted {
                    if let Err(e) = self.reload_rules().await {
                        log::warn!("rule hot-reload after review failed: {e}");
                    }
                }
                self.send(ok_response(
                    request_id,
                    json!({
                        "reviewed": true,
                        "proposal_id": outcome.proposal_id,
                        // The proposal disposition (accepted/rejected) — kept for the dashboard.
                        "resulting_status": outcome.new_status.as_str(),
                        // The resulting rule mode (shadow_mode / active), or null when no rule was
                        // created. This is what the UI shows as "now active" vs "shadow-testing".
                        "rule_status": outcome.rule_status.map(|s| s.as_str()),
                        "rule_id": outcome.created_rule_id,
                    }),
                ))
            }
            Err(e) => self.send(error_response(
                request_id,
                "review_proposal_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Return the effective, secret-free settings snapshot — derived live from the mutable config,
    /// enriched with each provider's `configured` flag (a secret-presence read; never the key
    /// itself). Async because the secret-presence check hits the secret store.
    async fn handle_get_settings(&self, request_id: String) -> Result<(), TransportError> {
        let Some(admin) = &self.admin else {
            return self.send(admin_not_configured(request_id));
        };
        let snapshot = admin.snapshot();
        let mut payload = serde_json::to_value(&snapshot).unwrap_or_else(|_| json!({}));
        // Stamp `configured` per provider from the 0600 store (does an API key exist?), so the UI
        // can render "•••• set" without the host ever returning the secret.
        if let Some(providers) = payload.get_mut("providers").and_then(Value::as_array_mut) {
            for entry in providers.iter_mut() {
                // `null` when the store couldn't be read — never a confident `false` that would
                // tell the user a real key is absent (which the UI shows as "•••• set" vs "no key").
                let configured = match entry.get("id").and_then(Value::as_str) {
                    Some(id) => admin.secret_is_set(id).await,
                    None => Some(false),
                };
                if let Value::Object(map) = entry {
                    map.insert("configured".to_owned(), json!(configured));
                }
            }
        }
        self.send(ok_response(request_id, payload))
    }

    /// Write the scalar config fields the snapshot exposes (retention level, follow-up tick,
    /// catch-up-on-launch). Validates host-side, mutates the live config, persists if it came
    /// from a file, and returns the fresh snapshot. Never widens the safety posture.
    async fn handle_set_settings(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(admin) = &self.admin else {
            return self.send(admin_not_configured(request_id));
        };
        // Apply under the lock, validate, then drop the lock before persisting.
        let validation = {
            let mut config = admin.config.lock().unwrap();
            if let Some(level) = payload.get("retention_level").and_then(Value::as_str) {
                match RetentionLevel::from_db_str(level) {
                    Some(l) => config.retention.level = l,
                    None => {
                        return self.send(error_response(
                            request_id,
                            "invalid_payload",
                            format!("unknown retention_level {level:?}"),
                            None,
                        ))
                    }
                }
            }
            // The onboarding consent gate: a body-retaining level is inert until this is granted
            // (see `AppConfig::retention_level`). Revoking it (or lowering the level below body
            // retention) makes the EFFECTIVE level metadata at once; the matching down-level
            // purge of already-stored bodies (`MessageRepository::purge_bodies`) is invoked once
            // the message store is wired into the router (Phase 2 — no bodies are persisted yet).
            if let Some(consent) = payload.get("body_consent").and_then(Value::as_bool) {
                config.retention.body_consent = consent;
            }
            if let Some(tick) = payload
                .get("follow_up_tick_seconds")
                .and_then(Value::as_u64)
            {
                config.followups.tick_seconds = tick;
            }
            if let Some(catch_up) = payload.get("catch_up_on_launch").and_then(Value::as_bool) {
                config.followups.catch_up_on_launch = catch_up;
            }
            config.validate().map_err(|e| e.to_string())
        };
        if let Err(e) = validation {
            return self.send(error_response(request_id, "invalid_payload", e, None));
        }
        // If this write touched the retention posture and the effective level no longer retains
        // bodies, purge any already-stored bodies at once — the privacy dial acts immediately.
        let retention_touched =
            payload.get("retention_level").is_some() || payload.get("body_consent").is_some();
        if retention_touched {
            self.purge_bodies_if_not_retaining(admin).await;
        }
        self.respond_settings_write(request_id, admin)
    }

    /// Set (or clear) the per-category action policy. Payload `{ category, policy }` where
    /// `policy` ∈ `auto`/`suggest`/`off`. The category must be a known, policy-targetable key (a
    /// starter category or a tag-mapped one) — a typo is rejected rather than silently stored
    /// against a category that never classifies. Setting `auto` REMOVES the entry (the map stays
    /// sparse: absent ⇒ the default `Auto`), so the surface never accretes redundant defaults.
    async fn handle_set_category_policy(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(admin) = &self.admin else {
            return self.send(admin_not_configured(request_id));
        };
        let Some(category) = payload
            .get("category")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return self.send(error_response(
                request_id,
                "invalid_payload",
                "set_category_policy requires a category",
                None,
            ));
        };
        let Some(policy) = payload
            .get("policy")
            .and_then(Value::as_str)
            .and_then(CategoryPolicy::parse)
        else {
            return self.send(error_response(
                request_id,
                "invalid_payload",
                "set_category_policy requires policy ∈ auto|suggest|off",
                None,
            ));
        };
        {
            let mut config = admin.config.lock().unwrap();
            if !config.triage.is_known_category(&category) {
                return self.send(error_response(
                    request_id,
                    "invalid_payload",
                    format!("unknown category {category:?}"),
                    None,
                ));
            }
            if policy == CategoryPolicy::Auto {
                config.triage.category_policies.remove(&category);
            } else {
                config.triage.category_policies.insert(category, policy);
            }
        }
        self.respond_settings_write(request_id, admin)
    }

    /// Set the per-account triage scope. Payload `{ account_id, enabled }`. An out-of-scope
    /// account (`enabled: false`) is classified but never auto-actioned or suggested (the same
    /// silence as a category `Off`). `enabled: true` REMOVES the entry (sparse map: absent ⇒ in
    /// scope), so the default in-scope posture is never spelled out redundantly.
    async fn handle_set_account_scope(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(admin) = &self.admin else {
            return self.send(admin_not_configured(request_id));
        };
        let Some(account_id) = payload
            .get("account_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
        else {
            return self.send(error_response(
                request_id,
                "invalid_payload",
                "set_account_scope requires a non-empty account_id",
                None,
            ));
        };
        let Some(enabled) = payload.get("enabled").and_then(Value::as_bool) else {
            return self.send(error_response(
                request_id,
                "invalid_payload",
                "set_account_scope requires a boolean enabled",
                None,
            ));
        };
        {
            let mut config = admin.config.lock().unwrap();
            if enabled {
                config.triage.account_scopes.remove(&account_id);
            } else {
                config.triage.account_scopes.insert(account_id, false);
            }
        }
        self.respond_settings_write(request_id, admin)
    }

    /// Set (or clear) a tag→category mapping. Payload `{ tag, category }`; an empty/absent
    /// `category` REMOVES the mapping. A non-empty category needs no pre-existing vocabulary entry
    /// — mapping a tag to a NEW category key is exactly how the user introduces a category — so it
    /// is accepted as-is (trimmed, lower-cased) and then surfaces in the vocabulary.
    async fn handle_set_tag_mapping(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(admin) = &self.admin else {
            return self.send(admin_not_configured(request_id));
        };
        let Some(tag) = payload
            .get("tag")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
        else {
            return self.send(error_response(
                request_id,
                "invalid_payload",
                "set_tag_mapping requires a non-empty tag",
                None,
            ));
        };
        let category = payload
            .get("category")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty());
        {
            let mut config = admin.config.lock().unwrap();
            match category {
                Some(category) => {
                    config.triage.tag_mappings.insert(tag, category);
                }
                None => {
                    config.triage.tag_mappings.remove(&tag);
                }
            }
            // Removing (or repointing) a mapping can orphan a tag-derived category's policy. Prune
            // it so the policy set never names a category the user can no longer see — and, more
            // importantly, so it can't silently reactivate if the tag is later remapped.
            config.triage.prune_orphaned_category_policies();
        }
        self.respond_settings_write(request_id, admin)
    }

    /// Engage/release the global pause kill-switch (host-side state, so it survives reloads and is
    /// enforced on the `new_mail` apply path — see [`is_paused`](Self::is_paused)).
    async fn handle_set_pause(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(admin) = &self.admin else {
            return self.send(admin_not_configured(request_id));
        };
        let paused = payload
            .get("paused")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        admin.config.lock().unwrap().paused = paused;
        // Distinguish a real save failure from the honest in-memory (no-file) case — a swallowed
        // error would let the user believe a safety-relevant setting survived to disk when it did
        // not (mirrors respond_settings_write).
        let persisted = admin.persist();
        let mut response =
            json!({ "paused": paused, "persisted": persisted.clone().unwrap_or(false) });
        if let (Err(e), Value::Object(map)) = (&persisted, &mut response) {
            map.insert("persist_error".to_owned(), json!(e));
        }
        self.send(ok_response(request_id, response))
    }

    /// Write (overwrite) a provider's API key directly into the 0600 secret store — the only path
    /// a key reaches disk, and the value is never returned. Clearing a key is not supported (the
    /// store has no delete); an empty secret is refused rather than stored as a blank key.
    async fn handle_set_secret(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(admin) = &self.admin else {
            return self.send(admin_not_configured(request_id));
        };
        let Some(provider_id) = payload.get("provider_id").and_then(Value::as_str) else {
            return self.send(error_response(
                request_id,
                "invalid_payload",
                "set_secret requires provider_id",
                None,
            ));
        };
        let secret = payload.get("secret").and_then(Value::as_str).unwrap_or("");
        if secret.is_empty() {
            return self.send(error_response(
                request_id,
                "invalid_payload",
                "set_secret requires a non-empty secret (clearing a key is not supported)",
                None,
            ));
        }
        match admin
            .secret_store
            .put(AdminSuite::secret_key(provider_id), Secret::new(secret))
            .await
        {
            Ok(()) => {
                // A newly-saved key can complete an otherwise-unauthenticated provider — rebuild
                // the live graph so the next draft uses it without a host restart.
                admin.rebuild_live_provider();
                self.send(ok_response(
                    request_id,
                    json!({ "stored": true, "provider_id": provider_id, "configured": true }),
                ))
            }
            Err(e) => self.send(error_response(
                request_id,
                "set_secret_failed",
                e.to_string(),
                None,
            )),
        }
    }

    /// Add / update / remove a provider entry and optionally choose the default. Pairs with
    /// `set_secret` (this writes only the public endpoint/kind, never the key). Validates the
    /// provider kind + default-consistency host-side before persisting.
    async fn handle_set_provider(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(admin) = &self.admin else {
            return self.send(admin_not_configured(request_id));
        };
        let Some(provider_id) = payload.get("provider_id").and_then(Value::as_str) else {
            return self.send(error_response(
                request_id,
                "invalid_payload",
                "set_provider requires provider_id",
                None,
            ));
        };
        let remove = payload
            .get("remove")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let validation = {
            let mut config = admin.config.lock().unwrap();
            if remove {
                config.ai.providers.retain(|p| p.id != provider_id);
                if config.ai.default_provider.as_deref() == Some(provider_id) {
                    config.ai.default_provider = None;
                }
            } else {
                let kind = payload.get("kind").and_then(Value::as_str);
                let endpoint = payload
                    .get("endpoint")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let model = payload
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                match config.ai.providers.iter_mut().find(|p| p.id == provider_id) {
                    Some(existing) => {
                        if let Some(kind) = kind {
                            existing.kind = kind.to_owned();
                        }
                        if payload.get("endpoint").is_some() {
                            existing.endpoint = endpoint;
                        }
                        if payload.get("model").is_some() {
                            existing.model = model;
                        }
                    }
                    None => config.ai.providers.push(ProviderSettings {
                        id: provider_id.to_owned(),
                        kind: kind.unwrap_or("openai_compatible").to_owned(),
                        endpoint,
                        model,
                    }),
                }
                if payload.get("set_default").and_then(Value::as_bool) == Some(true) {
                    config.ai.default_provider = Some(provider_id.to_owned());
                }
            }
            config.validate().map_err(|e| e.to_string())
        };
        if let Err(e) = validation {
            return self.send(error_response(request_id, "invalid_payload", e, None));
        }
        // The provider set/default changed — rebuild the live graph so the new default (or its
        // removal, degrading to unavailable) takes effect on the next draft, no restart needed.
        admin.rebuild_live_provider();
        self.respond_settings_write(request_id, admin)
    }

    /// List the models a provider's endpoint currently serves, so the options UI can offer a
    /// pick-list instead of a free-text field. `kind` + `endpoint` come from the request (the user
    /// may be probing an endpoint not yet saved); an optional `provider_id` resolves a saved
    /// provider's kind/endpoint when those are omitted, and attaches its stored API key for an
    /// authenticated cloud catalog. The provider-specific URL/shape lives in `mailmate-ai`; a
    /// transport failure degrades to a `list_models_failed` error beside the still-usable field.
    async fn handle_list_models(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(admin) = &self.admin else {
            log::warn!("list_models: admin surface not configured");
            return self.send(admin_not_configured(request_id));
        };
        let (kind, endpoint, api_key) = match admin.resolve_discovery_target(&payload).await {
            Ok(target) => target,
            Err(tail) => {
                return self.send(error_response(
                    request_id,
                    "invalid_payload",
                    format!("list_models {tail}"),
                    None,
                ));
            }
        };
        log::info!(
            "list_models: kind={kind} endpoint={endpoint} keyed={}",
            api_key.is_some()
        );
        match list_models(&kind, &endpoint, api_key.as_ref(), admin.http.as_ref()).await {
            Ok(models) => {
                log::info!("list_models ok: {} model(s) from {endpoint}", models.len());
                self.send(ok_response(
                    request_id,
                    json!({ "models": models, "kind": kind, "endpoint": endpoint }),
                ))
            }
            Err(e) => {
                log::warn!("list_models failed: kind={kind} endpoint={endpoint}: {e}");
                self.send(error_response(
                    request_id,
                    "list_models_failed",
                    e.to_string(),
                    Some(json!({ "kind": kind })),
                ))
            }
        }
    }

    /// Report the drafting provider posture for the compose-review panel: whether a default is
    /// configured, whether it actually *resolves* to a usable adapter (vs degrading to the
    /// zero-provider sentinel), and the default's public identity (id/kind/endpoint/model, plus
    /// whether a key is on file). It NEVER returns a key — only `secret_set` presence. The
    /// `available` flag is computed by [`build_provider`] itself, so it can never drift from what
    /// the draft path would actually get.
    async fn handle_provider_status(&self, request_id: String) -> Result<(), TransportError> {
        let Some(admin) = &self.admin else {
            return self.send(admin_not_configured(request_id));
        };
        // Clone the AI settings out and drop the lock before any await.
        let ai = admin.config.lock().unwrap().ai.clone();
        let default_id = ai.default_provider.clone();

        // The single source of truth for "would the draft path get a real provider?": run the same
        // construction predicate build_provider uses (structural, not an id-string match — a
        // provider named "unavailable" can't fool it). Construction is local (no network).
        let available =
            provider_is_configured(&ai, admin.secret_store.as_ref(), admin.http.clone());

        // The default provider's public identity (never its key) + whether a key is stored.
        let mut provider_json = Value::Null;
        if let Some(id) = &default_id {
            if let Some(p) = ai.providers.iter().find(|p| &p.id == id) {
                // Only the OpenAI-compatible adapter reads a key (and even then it's optional —
                // a local server may need none); the local adapters never do.
                let accepts_key = p.kind == "openai_compatible";
                let secret_set = admin.secret_is_set(id).await;
                provider_json = json!({
                    "id": p.id,
                    "kind": p.kind,
                    "endpoint": p.endpoint,
                    "model": p.model,
                    "accepts_key": accepts_key,
                    "secret_set": secret_set,
                });
            }
        }

        self.send(ok_response(
            request_id,
            json!({
                "configured": default_id.is_some(),
                "available": available,
                "default_provider": default_id,
                "provider": provider_json,
                "provider_count": ai.providers.len(),
            }),
        ))
    }

    /// Probe a provider's endpoint for reachability (the settings "Test connection" button),
    /// reusing the model-discovery transport. Resolution mirrors `list_models` (explicit
    /// kind+endpoint, or a saved `provider_id`). Reachability is reported as DATA on an `ok`
    /// response — `reachable: true/false` with the model count or the transport error — because
    /// "the endpoint is down" is a successful test result, not a host failure.
    async fn handle_test_provider(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let Some(admin) = &self.admin else {
            return self.send(admin_not_configured(request_id));
        };
        let (kind, endpoint, api_key) = match admin.resolve_discovery_target(&payload).await {
            Ok(target) => target,
            Err(tail) => {
                return self.send(error_response(
                    request_id,
                    "invalid_payload",
                    format!("test_provider {tail}"),
                    None,
                ));
            }
        };
        match list_models(&kind, &endpoint, api_key.as_ref(), admin.http.as_ref()).await {
            Ok(models) => self.send(ok_response(
                request_id,
                json!({
                    "reachable": true,
                    "kind": kind,
                    "endpoint": endpoint,
                    "model_count": models.len(),
                }),
            )),
            Err(e) => self.send(ok_response(
                request_id,
                json!({
                    "reachable": false,
                    "kind": kind,
                    "endpoint": endpoint,
                    "error": e.to_string(),
                }),
            )),
        }
    }

    /// Persist (best-effort) and answer a config write with the fresh snapshot + whether it saved.
    fn respond_settings_write(
        &self,
        request_id: String,
        admin: &AdminSuite,
    ) -> Result<(), TransportError> {
        let persisted = admin.persist();
        let snapshot = admin.snapshot();
        let mut payload = serde_json::to_value(&snapshot).unwrap_or_else(|_| json!({}));
        if let Value::Object(map) = &mut payload {
            map.insert("updated".to_owned(), json!(true));
            map.insert(
                "persisted".to_owned(),
                json!(persisted.clone().unwrap_or(false)),
            );
        }
        // A save failure is surfaced (the in-memory write still took effect) rather than hidden.
        if let Err(e) = persisted {
            if let Value::Object(map) = &mut payload {
                map.insert("persist_error".to_owned(), json!(e));
            }
        }
        self.send(ok_response(request_id, json!({ "settings": payload })))
    }

    /// Answer the `hello` handshake: a richer-than-`ping` round-trip carrying the host/protocol
    /// version, the capabilities this build has wired, and the secret-free drafting/retention
    /// posture. The extension uses it to render Connected / version-mismatch and to gate
    /// feature UI *before* sending any real request (and to guard the single-writer channel
    /// against a protocol it cannot speak). It exposes no secret — drafting availability is a
    /// bool, not a key.
    fn handle_hello(&self, request_id: String) -> Result<(), TransportError> {
        let mut capabilities = vec![
            "classify_message",
            "draft_reply",
            "regenerate_draft",
            "record_user_action",
            "explain_decision",
            "list_recent_activity",
            "review_rule_proposal",
            "triage_existing_mail",
        ];
        if self.followups.is_some() {
            capabilities.push("followups");
            capabilities.push("list_followups");
        }
        if self.rule_reload.is_some() {
            // The Rules manager needs the rule store (wired with the composition root).
            capabilities.push("list_rules");
            capabilities.push("set_rule_status");
        }
        if self.admin.is_some() {
            capabilities.push("list_pending_reviews");
            capabilities.push("get_settings");
            capabilities.push("set_settings");
            capabilities.push("set_category_policy");
            capabilities.push("set_account_scope");
            capabilities.push("set_tag_mapping");
            capabilities.push("set_pause");
            capabilities.push("set_secret");
            capabilities.push("set_provider");
            capabilities.push("list_models");
            capabilities.push("provider_status");
            capabilities.push("test_provider");
        }
        // Drafting/retention come from the live config; an unwired admin surface reports the
        // host's safe local-first defaults (no provider, metadata retention).
        let (drafting_available, retention_level) = self.admin.as_ref().map_or_else(
            || (false, "metadata".to_owned()),
            |admin| {
                let snap = admin.snapshot();
                (snap.default_provider.is_some(), snap.retention_level)
            },
        );
        self.send(ok_response(
            request_id,
            json!({
                "host_version": env!("CARGO_PKG_VERSION"),
                "protocol_version": SUPPORTED_PROTOCOL_VERSION,
                "capabilities": capabilities,
                "drafting_available": drafting_available,
                "retention_level": retention_level,
            }),
        ))
    }

    /// Run the catch-up-on-launch drain: surface a `followup_draft_ready` for each fired step
    /// (a review-required draft, never sent on arrival) and a `followup_needs_attention` for
    /// each stale instance. **At most one batch cap** of instances per call (so a periodic tick
    /// can't storm). Returns how many instances this pass advanced (fired + needs-attention) — the
    /// launch catch-up loops on this (see [`drain_followups_to_empty`](Self::drain_followups_to_empty)).
    /// Host-initiated (not a request).
    ///
    /// # Errors
    /// Propagates a [`TransportError`] from emitting a frame. A scheduler/storage failure is
    /// audited and ends the sweep without erroring the channel.
    pub async fn drain_followups(&self) -> Result<usize, TransportError> {
        let Some(followups) = &self.followups else {
            return Ok(0);
        };
        let report = match followups
            .scheduler
            .drain_due(self.clock.now(), self.followup_batch_cap)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let _ = self
                    .audit
                    .append(
                        AuditEntry::new("followup_drain_failed", Actor::System)
                            .with_payload(json!({ "error": e.to_string() })),
                    )
                    .await;
                return Ok(0);
            }
        };
        let advanced = report.fired.len() + report.needs_attention.len();
        for fired in &report.fired {
            self.send(Frame::Notification {
                protocol_version: ProtocolVersion::default(),
                notification_id: format!("ntf_followup_{}", fired.workflow_instance_id),
                type_: "followup_draft_ready".to_owned(),
                payload: followup_draft_ready_payload(fired),
            })?;
        }
        for item in &report.needs_attention {
            self.send(Frame::Notification {
                protocol_version: ProtocolVersion::default(),
                notification_id: format!("ntf_attention_{}", item.workflow_instance_id),
                type_: "followup_needs_attention".to_owned(),
                payload: followup_needs_attention_payload(item),
            })?;
        }
        Ok(advanced)
    }

    /// Drain follow-ups repeatedly until a pass advances nothing — the launch catch-up, so a
    /// backlog accumulated while offline fully clears even with no periodic tick. Each pass is
    /// batch-capped; the iteration ceiling backstops termination (a pass that selects instances but
    /// advances none — the `current_step_index` footgun — returns 0 and stops the loop).
    ///
    /// # Errors
    /// Propagates a [`TransportError`] from emitting a frame.
    pub async fn drain_followups_to_empty(&self) -> Result<(), TransportError> {
        for _ in 0..MAX_CATCH_UP_PASSES {
            if self.drain_followups().await? == 0 {
                break;
            }
        }
        Ok(())
    }

    /// Mine recurring feedback for deterministic rule candidates and surface any NEW proposals
    /// for review — the learning loop's proactive half. Runs at launch (catch-up) and on the
    /// periodic tick (see [`runtime::serve`](crate::runtime::serve)). Entirely model-free: the
    /// clustering + thresholds are deterministic, so it proposes with zero providers configured.
    /// Each freshly-persisted proposal emits a `proposal_ready` notification; the candidate never
    /// auto-applies — it lands `pending_review` and routes through the Proposals tab + shadow gate.
    ///
    /// Seed the cold-start **starter rules** (§3.5) as fresh drafts, so a brand-new install has
    /// something concrete to review and one-tap activate before any correction exists. Idempotent
    /// across launches — a name collision is skipped, never overwritten — so re-running on every
    /// start adds nothing once seeded. A no-op when the rule store is not wired (a router built
    /// without the composition root). Returns the import summary (how many newly seeded vs already
    /// present).
    ///
    /// # Errors
    /// Propagates a [`StorageError`](mailmate_common::error::StorageError) if a rule write fails for
    /// a non-collision reason.
    pub async fn bootstrap_starter_rules(
        &self,
    ) -> Result<ImportSummary, mailmate_common::error::StorageError> {
        let Some(reload) = &self.rule_reload else {
            return Ok(ImportSummary::default());
        };
        ImportExportService::new(reload.rules.clone())
            .import(
                &crate::bootstrap::starter_rules_manifest(),
                crate::bootstrap::STARTER_PREFIX,
            )
            .await
    }

    /// # Errors
    /// Propagates a [`TransportError`] from emitting a frame. A storage failure inside the engine
    /// is audited and ends the pass without erroring the channel (mirrors [`drain_followups`]).
    ///
    /// [`drain_followups`]: Self::drain_followups
    pub async fn generate_proposals(&self) -> Result<(), TransportError> {
        let proposals = match self
            .learning
            .propose_candidates(ProposalTrigger::all())
            .await
        {
            Ok(proposals) => proposals,
            Err(e) => {
                let _ = self
                    .audit
                    .append(
                        AuditEntry::new("proposal_generation_failed", Actor::System)
                            .with_payload(json!({ "error": e.to_string() })),
                    )
                    .await;
                return Ok(());
            }
        };
        for proposal in &proposals {
            self.send(Frame::Notification {
                protocol_version: ProtocolVersion::default(),
                notification_id: format!("ntf_proposal_{}", proposal.id),
                type_: "proposal_ready".to_owned(),
                payload: proposal_ready_payload(proposal),
            })?;
        }
        Ok(())
    }

    /// Route a recorded user action to its single owner: a correction to its feedback table,
    /// or a provenance/execution fact to the audit log.
    async fn handle_record(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let parsed: RecordUserActionPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };

        let ack = match self.route_recorded(&parsed).await {
            Ok((sink, id)) => ok_response(
                request_id,
                json!({ "recorded": true, "sink": sink, "id": id }),
            ),
            Err(e) => error_response(request_id, "record_failed", e, None),
        };
        self.send(ack)
    }

    /// Record outbound (sent) mail as VIP/priority evidence: one `mail_sent` audit row per
    /// recipient domain. A later proposal pass counts these per domain and surfaces a human-gated
    /// VIP rule for the domains the user emails most (learn-from-Sent). Recipients without a
    /// parseable domain are skipped; the ACK reports how many rows were recorded.
    async fn handle_record_sent_mail(
        &self,
        request_id: String,
        payload: Value,
    ) -> Result<(), TransportError> {
        let parsed: SentMailPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => return self.send(invalid_payload(request_id, &e)),
        };
        // Dedupe recipients within one send so emailing two people @acme.com counts the domain once
        // for that message (a single send is one outbound interaction with the domain, not two).
        let mut domains: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for recipient in &parsed.recipients {
            if let Some(domain) = recipient_domain(recipient) {
                domains.insert(domain);
            }
        }
        let mut recorded = 0usize;
        for domain in domains {
            let entry = AuditEntry::new(event_type::MAIL_SENT, Actor::User).with_payload(json!({
                "recipient_domain": domain,
                "subject": parsed.subject,
            }));
            if self.audit.append(entry).await.is_ok() {
                recorded += 1;
            }
        }
        self.send(ok_response(request_id, json!({ "recorded": recorded })))
    }

    /// The routing decision for one recorded action. Returns `(sink, id)`.
    async fn route_recorded(
        &self,
        payload: &RecordUserActionPayload,
    ) -> Result<(&'static str, String), String> {
        match payload.event_type.as_str() {
            "junk_changed" => {
                let message_id = payload
                    .message_id()
                    .ok_or("junk_changed requires thunderbird_message_id")?;
                // The junk state is the discriminator between MarkSpam and MarkNotSpam, so it is
                // required: a missing field must error, not silently fabricate a spam label (which
                // would also feed the Tier-2 online update).
                let junk = payload.junk.ok_or("junk_changed requires junk")?;
                let features = self.cached_features(&message_id);
                let correction = if junk {
                    UserCorrection::MarkSpam { message_id }
                } else {
                    UserCorrection::MarkNotSpam { message_id }
                };
                let id = self
                    .correction
                    .handle_correction(correction, features, self.correction_ctx(payload))
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(("classification_feedback", id.into_string()))
            }
            "message_moved" if payload.user_initiated => {
                let message_id = payload
                    .message_id()
                    .ok_or("message_moved requires thunderbird_message_id")?;
                let to_folder = payload
                    .to_folder_id
                    .clone()
                    .map(FolderId::from)
                    .ok_or("message_moved requires to_folder_id")?;
                let features = self.cached_features(&message_id);
                let correction = UserCorrection::LearnFiling {
                    message_id,
                    to_folder,
                };
                let id = self
                    .correction
                    .handle_correction(correction, features, self.correction_ctx(payload))
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(("filing_feedback", id.into_string()))
            }
            // A tag add/remove → a first-class category signal in classification feedback.
            "tag_changed" => {
                let message_id = payload
                    .message_id()
                    .ok_or("tag_changed requires thunderbird_message_id")?;
                let tag = payload.tag.clone().ok_or("tag_changed requires tag")?;
                // The direction is required: a missing `added` must error, not silently pick a
                // polarity (mirrors the junk-without-junk guard — never fabricate a signal).
                let added = payload.added.ok_or("tag_changed requires added")?;
                let features = self.cached_features(&message_id);
                let id = self
                    .correction
                    .handle_correction(
                        UserCorrection::TagChanged {
                            message_id,
                            tag,
                            added,
                        },
                        features,
                        self.correction_ctx(payload),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(("classification_feedback", id.into_string()))
            }
            // A one-click wrong-category correction → classification feedback.
            "classification_corrected" => self.route_label_correction(payload).await,
            // The user rejected one of the verdict's salient signals from the panel.
            "signal_marked_wrong" => self.route_signal_marked_wrong(payload).await,
            // An undo of an auto-applied action → strong negative learning signal.
            "action_undone" => self.route_undo(payload).await,
            // A dismissed suggestion is the ignore-rate signal. The feedback tables key on a
            // *chosen* label/folder, which a dismiss does not supply; fabricating one would be
            // a false signal (cf. the junk-without-junk guard), so it is captured as audit
            // provenance — queryable for ignore/dismiss-rate — carrying action_kind + authored_by.
            "suggestion_dismissed" => self.audit_recorded(payload).await,
            // The user sent a MailMate draft they had edited — the edit-divergence signal. A draft
            // is not a classification (no chosen label/folder to key a feedback row on), so it is
            // recorded as audit provenance carrying the `draft_id`: queryable for a draft edit-rate
            // without fabricating a learned-task signal.
            "draft_diverged" => self.audit_recorded(payload).await,
            // A reply on a tracked thread exits the follow-up sequence (when follow-ups are
            // wired); otherwise it falls through to the audit arm as plain provenance.
            "reply_received" if self.followups.is_some() => self.route_reply(payload).await,
            "bounce_received" if self.followups.is_some() => self.route_bounce(payload).await,
            // Everything else is pure provenance / an execution result: audit only.
            _ => self.audit_recorded(payload).await,
        }
    }

    /// Record an event as plain audit provenance (the non-correction arm): an execution
    /// result, a pure observation, or a signal with no learned-task feedback table. Returns
    /// `("audit", id)`.
    async fn audit_recorded(
        &self,
        payload: &RecordUserActionPayload,
    ) -> Result<(&'static str, String), String> {
        // Attribute by origin: an execution result is the extension's; a move that reaches this
        // arm is host-initiated (a user move took the correction arm above), so it is the
        // system's; the rest are user-observed behavior.
        let event = payload.event_type.as_str();
        let actor = match event {
            "action_applied" | "action_failed" => Actor::Extension,
            "message_moved" => Actor::System,
            _ => Actor::User,
        };
        let mut entry = AuditEntry::new(event, actor).with_payload(provenance_payload(payload));
        if let Some(message_id) = payload.message_id() {
            entry = entry.with_message(message_id);
        }
        // Stamp the rule on the audit row's indexed column (not just the payload JSON) when the
        // event names one — an Undo carries the rule whose auto-applied action it reverses. This is
        // what makes the Rules-manager's per-rule correction count queryable (`AuditQuery::rule_id`).
        // The undo of an auto-applied effect is always an action rule, so the kind is `Action`.
        if let Some(rule_id) = payload.rule_id.as_deref().filter(|s| !s.is_empty()) {
            entry = entry.with_rule(RuleKind::Action, RuleId::from(rule_id));
        }
        let id = self.audit.append(entry).await.map_err(|e| e.to_string())?;
        Ok(("audit", id.into_string()))
    }

    /// Route a one-click wrong-category correction into classification feedback. The prior
    /// label flows in as the AI label so the captured row's polarity records the override
    /// honestly (a diverging label is `Negative`, an agreeing one is reinforcement).
    async fn route_label_correction(
        &self,
        payload: &RecordUserActionPayload,
    ) -> Result<(&'static str, String), String> {
        let message_id = payload
            .message_id()
            .ok_or("classification_corrected requires thunderbird_message_id")?;
        let label = payload
            .corrected_label
            .clone()
            .ok_or("classification_corrected requires corrected_label")?;
        let mut context = self.correction_ctx(payload);
        context.ai_label = payload.prior_label.clone();
        let features = self.cached_features(&message_id);
        let id = self
            .correction
            .handle_correction(
                UserCorrection::CorrectLabel { message_id, label },
                features,
                context,
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(("classification_feedback", id.into_string()))
    }

    /// Route a rejected salient signal (the user clicked "this reason is wrong" on a panel chip)
    /// into classification feedback as negative evidence keyed on the signal id. The prior
    /// verdict label rides in as the AI label so the row records which verdict the reason was
    /// rejected against. The full feature vector is recalled from the classify cache so the
    /// captured row keys on what the model actually saw.
    async fn route_signal_marked_wrong(
        &self,
        payload: &RecordUserActionPayload,
    ) -> Result<(&'static str, String), String> {
        let message_id = payload
            .message_id()
            .ok_or("signal_marked_wrong requires thunderbird_message_id")?;
        let signal_id = payload
            .signal_id
            .clone()
            .ok_or("signal_marked_wrong requires signal_id")?;
        let mut context = self.correction_ctx(payload);
        context.ai_label = payload.prior_label.clone();
        let features = self.cached_features(&message_id);
        let id = self
            .correction
            .handle_correction(
                UserCorrection::SignalMarkedWrong {
                    message_id,
                    signal_id,
                },
                features,
                context,
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(("classification_feedback", id.into_string()))
    }

    /// Route an undo of an auto-applied action into learning as strong negative evidence
    /// against the rule that fired. A reverted *move* teaches the filing it was put back to
    /// (the rule's now-undone target is the diverged AI suggestion → negative for that
    /// folder); an undone *junk-mark* is a not-spam correction; a reverted tag/draft has no
    /// learned-task feedback table, so it is recorded as provenance.
    /// Best-effort: record an undo as audit provenance stamped with the rule it reverses (on the
    /// indexed `rule_id` column), so the Rules manager can count corrections per rule. This is the
    /// metric/provenance copy; the strong learning signal still flows to the feedback tables. Never
    /// fails the undo — a metric-row write error is swallowed.
    async fn record_undo_provenance(
        &self,
        payload: &RecordUserActionPayload,
        message_id: &MessageId,
    ) {
        let Some(rule_id) = payload.rule_id.as_deref().filter(|s| !s.is_empty()) else {
            return;
        };
        let entry = AuditEntry::new(event_type::ACTION_UNDONE, Actor::User)
            .with_payload(provenance_payload(payload))
            .with_message(message_id.clone())
            .with_rule(RuleKind::Action, RuleId::from(rule_id));
        // Best-effort: the undo's learning signal must not fail on a metric-row write. But a
        // SILENT loss would undercount the rule and read as a confident "0 corrections" — exactly
        // the misleading zero the manager promises to avoid — so a failure is logged, not hidden.
        if let Err(e) = self.audit.append(entry).await {
            log::warn!(
                "undo provenance audit write failed (rule correction count may undercount): {e}"
            );
        }
    }

    async fn route_undo(
        &self,
        payload: &RecordUserActionPayload,
    ) -> Result<(&'static str, String), String> {
        let message_id = payload
            .message_id()
            .ok_or("action_undone requires thunderbird_message_id")?;
        match payload.action_kind.as_deref() {
            Some("move") => {
                let to_folder =
                    payload.to_folder_id.clone().map(FolderId::from).ok_or(
                        "action_undone(move) requires to_folder_id (the reverted-to folder)",
                    )?;
                let mut context = self.correction_ctx(payload);
                // The rule's now-undone target (where the message was) is the AI-suggested
                // folder the human diverged from by reverting it.
                context.ai_suggested_folder = payload.from_folder_id.clone().map(FolderId::from);
                let features = self.cached_features(&message_id);
                self.record_undo_provenance(payload, &message_id).await;
                let id = self
                    .correction
                    .handle_correction(
                        UserCorrection::LearnFiling {
                            message_id,
                            to_folder,
                        },
                        features,
                        context,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(("filing_feedback", id.into_string()))
            }
            // A rule only ever auto-*marks* junk; undoing it is a not-spam correction.
            Some("mark_junk" | "junk") => {
                let features = self.cached_features(&message_id);
                self.record_undo_provenance(payload, &message_id).await;
                let id = self
                    .correction
                    .handle_correction(
                        UserCorrection::MarkNotSpam { message_id },
                        features,
                        self.correction_ctx(payload),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(("classification_feedback", id.into_string()))
            }
            _ => self.audit_recorded(payload).await,
        }
    }

    /// Exit every tracked sequence on the reply's thread (host-side thread identity). The
    /// guard in `route_recorded` ensures the follow-up suite is present before this is called.
    async fn route_reply(
        &self,
        payload: &RecordUserActionPayload,
    ) -> Result<(&'static str, String), String> {
        let followups = self
            .followups
            .as_ref()
            .ok_or("reply_received requires the follow-up suite")?;
        let thread = payload
            .thread_id
            .clone()
            .ok_or("reply_received requires thread_id")?;
        let thread_id = ThreadId::from(thread);
        let items = followups
            .pipeline_items
            .get_by_thread(&thread_id)
            .await
            .map_err(|e| e.to_string())?;
        let mut exited = Vec::new();
        for item in items {
            let item_id = item.id.clone();
            let ids = followups
                .exit_detector
                .on_exit_event(item.id, ExitEvent::ReplyReceived)
                .await
                .map_err(|e| e.to_string())?;
            self.audit_exit(&item_id, ExitEvent::ReplyReceived, &ids)
                .await;
            exited.extend(
                ids.into_iter()
                    .map(mailmate_common::ids::WorkflowInstanceId::into_string),
            );
        }
        Ok(("workflow_exit", exited.join(",")))
    }

    /// Exit every tracked sequence on a bounced (NDR) thread. The host **re-confirms** the bounce
    /// from the reported sender + subject before exiting — a `bounce_received` event whose message
    /// does not look like a delivery-failure notification is declined (`bounce_unconfirmed`,
    /// audited, no exit), so a mis-tagged or spoofed event can't silently kill a live sequence.
    async fn route_bounce(
        &self,
        payload: &RecordUserActionPayload,
    ) -> Result<(&'static str, String), String> {
        let followups = self
            .followups
            .as_ref()
            .ok_or("bounce_received requires the follow-up suite")?;
        let thread = payload
            .thread_id
            .clone()
            .ok_or("bounce_received requires thread_id")?;

        // Re-confirm it really is a delivery-failure notification (defense in depth).
        let sender = payload.sender_email.as_deref().unwrap_or("");
        let subject = payload.subject.as_deref().unwrap_or("");
        if !mailmate_workflow::bounce::is_bounce_notification(sender, subject) {
            let _ = self
                .audit
                .append(
                    AuditEntry::new("bounce_unconfirmed", Actor::System).with_payload(json!({
                        "thread_id": thread,
                        "sender_email": sender,
                        "subject": subject,
                    })),
                )
                .await;
            return Ok(("bounce_unconfirmed", String::new()));
        }

        let thread_id = ThreadId::from(thread);
        let items = followups
            .pipeline_items
            .get_by_thread(&thread_id)
            .await
            .map_err(|e| e.to_string())?;
        let mut exited = Vec::new();
        for item in items {
            let item_id = item.id.clone();
            let ids = followups
                .exit_detector
                .on_exit_event(item.id, ExitEvent::Bounced)
                .await
                .map_err(|e| e.to_string())?;
            self.audit_exit(&item_id, ExitEvent::Bounced, &ids).await;
            exited.extend(
                ids.into_iter()
                    .map(mailmate_common::ids::WorkflowInstanceId::into_string),
            );
        }
        Ok(("workflow_exit", exited.join(",")))
    }

    /// Build the correction context from the wire payload's optional hints.
    fn correction_ctx(&self, payload: &RecordUserActionPayload) -> CorrectionContext {
        // The account is whatever the extension sent, else the one cached when this message was
        // classified — so a correction scopes per account even if the wire omits it. `None` only
        // when both are unknown (an aged-out message with no payload account), degrading to Global.
        let account_id = payload.account_id.clone().or_else(|| {
            payload
                .message_id()
                .and_then(|id| self.cached_account(&id))
                .map(|account| account.as_str().to_owned())
        });
        CorrectionContext {
            sender_domain: payload.sender_domain.clone(),
            account_id,
            ai_suggested_folder: payload.ai_suggested_folder.clone().map(FolderId::from),
            ..CorrectionContext::default()
        }
    }

    /// Send one frame through the output transport.
    fn send(&self, frame: Frame) -> Result<(), TransportError> {
        self.out.send(frame)
    }
}

/// A schema-parse failure becomes a correlated `invalid_payload` error response.
fn invalid_payload(request_id: String, err: &serde_json::Error) -> Frame {
    error_response(
        request_id,
        "invalid_payload",
        format!("payload did not match the expected schema: {err}"),
        None,
    )
}

/// An `invalid_payload` error carrying a specific human reason (for hand-validated payloads that
/// don't deserialize through a typed DTO).
fn invalid_payload_msg(request_id: String, message: &str) -> Frame {
    error_response(request_id, "invalid_payload", message, None)
}

/// The error response for a follow-up request when the suite is not wired (the binary's
/// composition root that injects it is deferred to Phase 12).
fn followups_not_configured(request_id: String) -> Frame {
    error_response(
        request_id,
        "followups_not_configured",
        "the follow-up workflow engine is not wired into this host build",
        None,
    )
}

/// The error response for an admin request when the management surface is not wired.
fn admin_not_configured(request_id: String) -> Frame {
    error_response(
        request_id,
        "admin_not_configured",
        "the management surface (proposals/settings) is not wired into this host build",
        None,
    )
}

/// The error response for a data-rights request when the erasure/export repository is not wired.
fn data_rights_not_configured(request_id: String) -> Frame {
    error_response(
        request_id,
        "data_rights_not_configured",
        "the delete-my-data / export surface is not wired into this host build",
        None,
    )
}

/// Map an erasure/export backend failure to an error response.
fn erasure_failed(request_id: String, e: &mailmate_common::error::StorageError) -> Frame {
    error_response(request_id, "data_rights_failed", e.to_string(), None)
}

/// The error response for a reminder request when the reminder store is not wired.
fn reminders_not_configured(request_id: String) -> Frame {
    error_response(
        request_id,
        "reminders_not_configured",
        "the remind-me / snooze surface is not wired into this host build",
        None,
    )
}

/// The JSON summary of a reminder, shared by `list_reminders` and the `reminder_due` nudge.
fn reminder_summary(reminder: &mailmate_common::reminder::Reminder) -> Value {
    json!({
        "reminder_id": reminder.id.as_str(),
        "message_id": reminder.message_id.as_ref().map(|m| m.as_str()),
        "thread_id": reminder.thread_id.as_ref().map(|t| t.as_str()),
        "account_id": reminder.account_id.as_ref().map(|a| a.as_str()),
        "title": reminder.title,
        "note": reminder.note,
        "due_at": reminder.due_at.to_rfc3339(),
        "status": reminder.status.as_str(),
    })
}

/// Does an audit entry's exact `event_type` belong to the requested Activity-tab *family*?
/// `None` (or the sentinel `"all"`, already stripped by the caller) matches everything. The
/// families mirror the dashboard's filter chips and group the host's concrete event types — a
/// failure variant lives with its family (a failed apply is still "applied"; a rejected
/// classification is still "classified") so the stream never silently hides a real event. An
/// unrecognised family is permissive (matches all) rather than blanking the view.
fn activity_family_matches(family: Option<&str>, event_type: &str) -> bool {
    let Some(family) = family else {
        return true;
    };
    match family {
        "classified" => matches!(
            event_type,
            "classified" | "classification_failed" | "new_mail_rejected"
        ),
        // Every spelling a failed apply can arrive under lives with the family: the host's own
        // `action_apply_failed` (over-the-wire send error) and the extension's `action_failed`
        // (the async Thunderbird-layer failure, the common case) — never hide a real failure.
        "applied" => matches!(
            event_type,
            "action_applied" | "action_apply_failed" | "action_failed"
        ),
        // A discarded model output is a refused action — it belongs with the policy-blocked family.
        "blocked" => matches!(
            event_type,
            "action_blocked_by_policy" | "provider_response_rejected"
        ),
        "corrected" => matches!(
            event_type,
            "suggestion_dismissed"
                | "classification_corrected"
                | "action_undone"
                | "junk_changed"
                | "message_moved"
        ),
        // Enrollment + exit (router-authored) AND the scheduler's per-step lifecycle events,
        // which share the same audit store — so a fired/coalesced/stale/failed step is never
        // dropped from the Follow-ups view.
        "follow_up" => matches!(
            event_type,
            "pipeline_item_enrolled"
                | "workflow_exited"
                | "followup_drain_failed"
                | "followup_step_fired"
                | "followup_coalesced"
                | "followup_needs_attention"
                | "followup_draft_failed"
                | "followup_version_missing"
        ),
        // Rule-proposal lifecycle, including the `workflow_status_changed` a review materializes.
        "proposal" => matches!(
            event_type,
            "rule_proposed"
                | "proposal_reviewed"
                | "rule_status_changed"
                | "rule_conflict_detected"
                | "workflow_status_changed"
        ),
        // An unknown filter must not blank the stream — show everything.
        _ => true,
    }
}

/// Does a deal match the Follow-ups view's `status_filter`? `None` (or `"all"`, already stripped)
/// matches everything. `won`/`lost` key on the deal's pipeline stage; `active`/`needs_attention`
/// key on its workflow-instance status. An unrecognised filter is permissive.
fn followup_status_matches(
    filter: Option<&str>,
    stage: mailmate_common::pipeline::PipelineStage,
    status: Option<WorkflowInstanceStatus>,
    needs_attention: bool,
) -> bool {
    use mailmate_common::pipeline::PipelineStage;
    let Some(filter) = filter else {
        return true;
    };
    match filter {
        "won" => stage == PipelineStage::Won,
        "lost" => stage == PipelineStage::Lost,
        "needs_attention" => needs_attention,
        "active" => status == Some(WorkflowInstanceStatus::Active),
        _ => true,
    }
}

/// Project a safe [`PlannedAction`] onto the [`MailAction`] the client applies, or `None` for
/// actions that are not a direct mail mutation (`CreateDraft`, `RequireReview`).
fn planned_to_mail_action(action: &PlannedAction) -> Option<MailAction> {
    match action {
        PlannedAction::Tag { message_id, tag } => Some(MailAction::Tag {
            message_id: message_id.clone(),
            tag: tag.clone(),
        }),
        PlannedAction::Move {
            message_id,
            to_folder,
        } => Some(MailAction::Move {
            message_id: message_id.clone(),
            to_folder: to_folder.clone(),
        }),
        PlannedAction::MarkJunk { message_id, junk } => Some(MailAction::MarkJunk {
            message_id: message_id.clone(),
            junk: *junk,
        }),
        PlannedAction::CreateDraft { .. } | PlannedAction::RequireReview { .. } => None,
    }
}

/// Silence an arrival entirely (a category `Off`, or an out-of-scope account): strip every
/// partition so nothing auto-applies and nothing is offered as a suggestion. The classification
/// verdict (and its safety findings) still rides on the notification — the user simply is not
/// asked to act on it. The blocked partition is cleared too: a category the user has turned off
/// should not even surface its refusals.
fn silence_actions(mut outcome: PlanningOutcome) -> PlanningOutcome {
    outcome.guarded_plan.allowed_actions.clear();
    outcome.guarded_plan.review_required_actions.clear();
    outcome.guarded_plan.blocked_actions.clear();
    outcome
}

/// Demote every would-be-auto-applied action to a suggestion (a category `Suggest`, or a global
/// pause): move the `allowed` actions into `review_required` so the notification surfaces them as
/// suggestions the user confirms, and nothing is auto-applied. Without this the allowed actions
/// would simply vanish on the background path (it lists `review_required`, not `allowed`), which
/// is what made the pre-Phase-5 pause silently drop them.
fn demote_to_suggestions(mut outcome: PlanningOutcome) -> PlanningOutcome {
    let mut allowed = std::mem::take(&mut outcome.guarded_plan.allowed_actions);
    outcome
        .guarded_plan
        .review_required_actions
        .append(&mut allowed);
    outcome
}

/// The inverse action that reverses `action` — the `reverses_to` the per-message Undo uses.
/// A move reverses to the message's `origin_folder`; an un-junk flips the junk bit. A tag add
/// has no safe-action inverse (there is no "untag" `PlannedAction`), so the extension reverses
/// those itself; a draft/review is not a mutation to undo.
fn reverse_of(action: &PlannedAction, origin_folder: &FolderId) -> Option<PlannedAction> {
    match action {
        PlannedAction::Move { message_id, .. } => Some(PlannedAction::Move {
            message_id: message_id.clone(),
            to_folder: origin_folder.clone(),
        }),
        PlannedAction::MarkJunk { message_id, junk } => Some(PlannedAction::MarkJunk {
            message_id: message_id.clone(),
            junk: !junk,
        }),
        PlannedAction::Tag { .. }
        | PlannedAction::CreateDraft { .. }
        | PlannedAction::RequireReview { .. } => None,
    }
}

/// The lower-cased domain of a recipient address — `"Boss <b@Acme.com>"`, `"b@acme.com"`, and
/// `"b@acme.com (work)"` all yield `acme.com`. The domain is the token after the LAST `@`, taken up
/// to the first delimiter (whitespace, `<` `>`, `;` `,`, or an RFC-5322 `(` comment), then validated
/// as a real domain (a dot, only `[a-z0-9.-]`). `None` for anything that isn't a parseable domain
/// (no `@`, a bare local part, `a@.com`, …) — we skip rather than mint a bogus VIP target that could
/// never match inbound mail.
fn recipient_domain(address: &str) -> Option<String> {
    let at = address.rfind('@')?;
    let domain: String = address[at + 1..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '>' | '<' | ';' | ',' | '(' | ')'))
        .collect::<String>()
        .trim_matches('.')
        .to_ascii_lowercase();
    let valid = domain.contains('.')
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    valid.then_some(domain)
}

/// The compact provenance payload an audited event records.
fn provenance_payload(p: &RecordUserActionPayload) -> Value {
    json!({
        "event_type": p.event_type,
        "thunderbird_message_id": p.thunderbird_message_id,
        "from_folder_id": p.from_folder_id,
        "to_folder_id": p.to_folder_id,
        "junk": p.junk,
        "read": p.read,
        "tag": p.tag,
        "user_initiated": p.user_initiated,
        "result": p.result,
        // Carried so the curator can compute ignore/undo-rate from the audit stream for
        // signals (dismissals, tag/draft undos) that have no learned-task feedback table.
        "action_kind": p.action_kind,
        "authored_by": p.authored_by,
        "rule_id": p.rule_id,
        // The draft a `draft_diverged` edit-divergence signal concerns.
        "draft_id": p.draft_id,
    })
}

#[cfg(test)]
mod recipient_domain_tests {
    use super::recipient_domain;

    #[test]
    fn parses_plain_and_angle_bracketed_addresses_case_folded() {
        assert_eq!(
            recipient_domain("boss@Acme.com").as_deref(),
            Some("acme.com")
        );
        assert_eq!(
            recipient_domain("Boss <boss@Acme.com>").as_deref(),
            Some("acme.com")
        );
    }

    #[test]
    fn strips_rfc_comments_and_trailing_punctuation() {
        assert_eq!(recipient_domain("a@b.com (work)").as_deref(), Some("b.com"));
        // Last address in a group/list with a trailing ';' keeps only the domain token.
        assert_eq!(recipient_domain("b@y.com;").as_deref(), Some("y.com"));
        assert_eq!(recipient_domain("a@x.com, ").as_deref(), Some("x.com"));
    }

    #[test]
    fn rejects_malformed_addresses_rather_than_minting_a_bogus_domain() {
        assert_eq!(recipient_domain("no-at-sign"), None);
        assert_eq!(recipient_domain("<@x>"), None, "no dot → not a domain");
        assert_eq!(recipient_domain("@x"), None);
        assert_eq!(recipient_domain("a@.com"), None, "empty label is invalid");
        assert_eq!(
            recipient_domain("a@b@c.com").as_deref(),
            Some("c.com"),
            "last @ wins"
        );
    }
}
