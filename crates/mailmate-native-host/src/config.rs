//! The host's TOML configuration: where data lives, the privacy-retention dial, follow-up
//! cadence driving, and the (forward-looking) AI provider list.
//!
//! This is the `[storage]`/`[retention]`/`[followups]`/`[ai]` file the architecture's
//! *Open Design Questions* call the day-one auditable substrate. It is a pure, serde-backed
//! value type: load it, validate it, map it into the concrete `StorageConfig`. The defaults
//! are MailMate's safe, local-first posture — an **in-memory-or-default-dir** SQLite DB,
//! `metadata` retention (no readable body stored), catch-up-on-launch follow-ups, and **no
//! AI provider** (fully functional with zero providers). Missing sections fall back to those
//! defaults, so an empty file is a valid, working config.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use mailmate_common::category::{starter_categories, CategoryDescriptor};
use mailmate_common::retention::RetentionLevel;
use mailmate_storage::{StorageConfig, StoragePath};

/// The provider-`kind` strings the config understands (matches the `mailmate-ai` adapters
/// plus the deterministic `mock`). Wiring a *network* provider additionally needs the edge
/// HTTP client; `mock` works offline.
pub const KNOWN_PROVIDER_KINDS: &[&str] = &[
    "ollama",
    "openai_compatible",
    "lm_studio",
    "llama_cpp",
    "mock",
];

/// The sentinel `database_path` selecting an ephemeral in-memory database.
pub const IN_MEMORY_DB: &str = "memory";

/// A failure loading, saving, or validating the config.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The config file could not be read or written.
    #[error("config I/O at {path}: {source}")]
    Io {
        /// The path involved.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The TOML could not be parsed.
    #[error("parsing config TOML: {0}")]
    Parse(String),
    /// The config could not be serialized to TOML.
    #[error("serializing config TOML: {0}")]
    Serialize(String),
    /// The config parsed but is semantically invalid.
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// The top-level host configuration.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct AppConfig {
    /// Where the database lives.
    pub storage: StorageSettings,
    /// The privacy-retention dial.
    pub retention: RetentionSettings,
    /// Follow-up cadence driving.
    pub followups: FollowupSettings,
    /// AI providers (forward-looking; the registry ships empty).
    pub ai: AiSettings,
    /// Per-category action policy, per-account triage scope, and tag→category mappings — the
    /// triage-tuning surface the Options page edits (`[triage]`). Empty by default: every
    /// category is `Auto` and every account is in scope.
    pub triage: TriageSettings,
    /// On-device Tier-2 training tuning (`[tier2]`): the correction-corpus ceiling and the
    /// held-out precision an artifact must clear before it activates.
    pub tier2: Tier2Settings,
    /// The global pause kill-switch: when true, the host stops auto-applying actions and firing
    /// follow-up drains (the dashboard/options Pause). Host-side state so it survives reloads.
    #[serde(default)]
    pub paused: bool,
}

/// `[storage]` — where the embedded database lives.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct StorageSettings {
    /// `"memory"` for an ephemeral in-memory database, or a filesystem path. When `None`
    /// (the default), the platform data directory is used (resolved at startup).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database_path: Option<String>,
}

/// `[retention]` — the privacy dial gating how much message content persists.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct RetentionSettings {
    /// The configured retention level (default `metadata`: no readable body stored).
    pub level: RetentionLevel,
    /// Affirmative consent to store readable bodies. Default `false`: even when `level` is a
    /// body-retaining level, the **effective** level stays `metadata` until the user has
    /// explicitly consented in onboarding. This is the consent gate that reconciles the
    /// recommended full-body default with the metadata-safe spec — full bodies are stored
    /// only after the user opts in.
    pub body_consent: bool,
}

/// `[followups]` — how the follow-up scheduler is driven.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct FollowupSettings {
    /// Drain overdue follow-ups once at startup (the catch-up-on-launch guarantee).
    pub catch_up_on_launch: bool,
    /// Seconds between periodic in-session drains; `0` disables the periodic tick (startup
    /// catch-up only).
    pub tick_seconds: u64,
    /// Max follow-up workflow instances fired in one drain pass — bounds a long-offline catch-up
    /// so it surfaces in capped batches over successive drains rather than one unbounded storm.
    pub drain_batch_cap: usize,
    /// Max durable reminders nudged in one drain pass (the notify-only remind-me / snooze surface).
    pub reminder_batch_cap: usize,
}

impl Default for FollowupSettings {
    fn default() -> Self {
        Self {
            catch_up_on_launch: true,
            tick_seconds: 0,
            drain_batch_cap: 100,
            reminder_batch_cap: 50,
        }
    }
}

/// `[tier2]` — on-device Tier-2 training tuning (Phase 8/9).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Tier2Settings {
    /// The newest-first ceiling on how many corrections feed one training run — bounds the
    /// in-memory corpus on a long-lived install.
    pub max_rows: usize,
    /// The held-out precision an artifact must clear before it activates (the safety gate that
    /// keeps a model the teacher until it has earned the predictor role). In `(0, 1]`.
    pub precision_gate: f64,
}

impl Default for Tier2Settings {
    fn default() -> Self {
        // Mirror the host's training-service defaults (`MAX_ROWS`, `DEFAULT_PRECISION_GATE`).
        Self {
            max_rows: 20_000,
            precision_gate: 0.8,
        }
    }
}

/// `[ai]` — provider configuration. The registry ships empty; this is the user's opt-in.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct AiSettings {
    /// The provider id used for LLM-always tasks. `None`/empty means "no provider" — LLM
    /// features degrade to review (MailMate stays fully functional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,
    /// The configured providers.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<ProviderSettings>,
}

/// One `[[ai.providers]]` entry.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProviderSettings {
    /// The provider's unique id (referenced by `default_provider`).
    pub id: String,
    /// The adapter kind (see [`KNOWN_PROVIDER_KINDS`]).
    pub kind: String,
    /// The endpoint URL (for network adapters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// The model name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// How MailMate treats actions on a message classified into a given category.
///
/// `Auto` (the default for any category without an explicit entry) keeps the full behaviour:
/// an active-rule-authored action auto-applies and everything else surfaces for review.
/// `Suggest` never auto-applies — every action becomes a suggestion the user confirms.
/// `Off` silences the category entirely: nothing auto-applies and no suggestions surface (the
/// verdict is still computed and recorded; the user is simply not asked to act on it).
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryPolicy {
    /// Auto-apply active-rule actions; surface the rest for review (the default posture).
    #[default]
    Auto,
    /// Never auto-apply; every action becomes a suggestion.
    Suggest,
    /// Silence the category — no auto-apply, no suggestions.
    Off,
}

impl CategoryPolicy {
    /// The wire/TOML token for this policy.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Suggest => "suggest",
            Self::Off => "off",
        }
    }

    /// Parse a wire token (`auto`/`suggest`/`off`) into a policy, `None` if unknown.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "auto" => Some(Self::Auto),
            "suggest" => Some(Self::Suggest),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    /// The more restrictive of two policies (`Off` > `Suggest` > `Auto`) — used to fold the
    /// global pause and the per-account scope into a per-category policy so the strictest wins.
    #[must_use]
    pub fn most_restrictive(self, other: Self) -> Self {
        self.max(other)
    }
}

// Ordering encodes restrictiveness: Auto < Suggest < Off (derived from the variant order), so
// `most_restrictive` is simply `max`. Pinned by a unit test.
impl PartialOrd for CategoryPolicy {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for CategoryPolicy {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}
impl CategoryPolicy {
    fn rank(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Suggest => 1,
            Self::Off => 2,
        }
    }
}

/// `[triage]` — the per-category / per-account / tag-mapping tuning the Options page edits.
/// Every map is sparse: an absent key means the default (a category is `Auto`, an account is in
/// scope). Stored in the config so it survives reloads and is auditable in the TOML.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct TriageSettings {
    /// Per-category action policy, keyed by the user-facing category key (the `starter_categories`
    /// keys plus any tag-mapped category). Absent ⇒ `Auto`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub category_policies: BTreeMap<String, CategoryPolicy>,
    /// Per-account triage scope, keyed by account id. Absent ⇒ in scope (`true`). A `false` entry
    /// takes the account out of scope: its arrivals are classified but never auto-actioned or
    /// suggested (the same silence as a category `Off`).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub account_scopes: BTreeMap<String, bool>,
    /// Tag→category mappings: a Thunderbird tag key → a category key. Surfaces the user's own tags
    /// as first-class categories (merged into the `get_settings` vocabulary), so a correction or a
    /// per-category policy can target them.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tag_mappings: BTreeMap<String, String>,
}

impl TriageSettings {
    /// The policy in force for a category key — the explicit entry, or `Auto` when none is set.
    #[must_use]
    pub fn policy_for(&self, category_key: &str) -> CategoryPolicy {
        self.category_policies
            .get(category_key)
            .copied()
            .unwrap_or_default()
    }

    /// Whether an account is in triage scope — `true` unless an explicit `false` entry exists.
    #[must_use]
    pub fn account_in_scope(&self, account_id: &str) -> bool {
        self.account_scopes.get(account_id).copied().unwrap_or(true)
    }

    /// The effective category vocabulary: the deterministic starter set plus any tag-mapped
    /// category key not already in it (so a tag→category mapping to a brand-new category surfaces
    /// it as a first-class, policy-targetable category). Stable order: starters first, then the
    /// derived keys sorted.
    #[must_use]
    pub fn effective_categories(&self) -> Vec<CategoryDescriptor> {
        let mut cats = starter_categories();
        let known: std::collections::BTreeSet<String> =
            cats.iter().map(|c| c.key.clone()).collect();
        let mut derived: Vec<String> = self
            .tag_mappings
            .values()
            .filter(|key| !key.is_empty() && !known.contains(*key))
            .cloned()
            .collect();
        derived.sort();
        derived.dedup();
        for key in derived {
            let label = humanize_category_key(&key);
            cats.push(CategoryDescriptor { key, label });
        }
        cats
    }

    /// Whether `category_key` is a known, policy-targetable category (a starter key or a
    /// tag-mapped one). Guards `set_category_policy` against a typo'd category.
    #[must_use]
    pub fn is_known_category(&self, category_key: &str) -> bool {
        self.effective_categories()
            .iter()
            .any(|c| c.key == category_key)
    }

    /// Drop any per-category policy whose category is no longer in the effective vocabulary —
    /// i.e. a tag-derived category whose last mapping was removed. Without this an orphaned
    /// policy would linger and, worse, **silently reactivate** if a new tag were later mapped to
    /// the same category key. Called after every tag-mapping change so the policy set never names
    /// a category the user can no longer see. Returns whether anything was pruned.
    pub fn prune_orphaned_category_policies(&mut self) -> bool {
        let known: std::collections::BTreeSet<String> = self
            .effective_categories()
            .into_iter()
            .map(|c| c.key)
            .collect();
        let before = self.category_policies.len();
        self.category_policies.retain(|key, _| known.contains(key));
        self.category_policies.len() != before
    }
}

/// Title-case a category key for display (`vip` → `Vip`, `cold-leads` → `Cold-leads`). A best-
/// effort humanization for a tag-derived category that has no curated label yet.
fn humanize_category_key(key: &str) -> String {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

impl AppConfig {
    /// Load and validate a config from a TOML file.
    ///
    /// # Errors
    /// [`ConfigError::Io`] if the file cannot be read, [`ConfigError::Parse`] on malformed
    /// TOML, or [`ConfigError::Invalid`] if validation fails.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let config: Self = toml::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Render the config as pretty TOML.
    ///
    /// # Errors
    /// [`ConfigError::Serialize`] if serialization fails (it should not for a valid config).
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(|e| ConfigError::Serialize(e.to_string()))
    }

    /// Write the config to `path` as TOML, creating the parent directory if needed.
    ///
    /// # Errors
    /// [`ConfigError::Serialize`] or [`ConfigError::Io`].
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = self.to_toml()?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
                    path: parent.display().to_string(),
                    source,
                })?;
            }
        }
        std::fs::write(path, text).map_err(|source| ConfigError::Io {
            path: path.display().to_string(),
            source,
        })
    }

    /// Check the config is internally consistent.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] on an unknown provider kind, a duplicate/empty provider id,
    /// or a `default_provider` that names no configured provider.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut seen = Vec::new();
        for provider in &self.ai.providers {
            if provider.id.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "a provider has an empty id".to_owned(),
                ));
            }
            if seen.contains(&provider.id) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate provider id {:?}",
                    provider.id
                )));
            }
            seen.push(provider.id.clone());
            if !KNOWN_PROVIDER_KINDS.contains(&provider.kind.as_str()) {
                return Err(ConfigError::Invalid(format!(
                    "provider {:?} has unknown kind {:?} (known: {})",
                    provider.id,
                    provider.kind,
                    KNOWN_PROVIDER_KINDS.join(", ")
                )));
            }
        }
        if let Some(default) = self.ai.default_provider.as_deref() {
            if !default.is_empty() && !seen.iter().any(|id| id == default) {
                return Err(ConfigError::Invalid(format!(
                    "default_provider {default:?} names no configured provider"
                )));
            }
        }
        // Triage maps: a hand-edited config bypasses the router handlers' guards, so an empty key
        // or value (which would otherwise round-trip silently and break lookups) is rejected here.
        for key in self.triage.category_policies.keys() {
            if key.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "a category_policies entry has an empty category key".to_owned(),
                ));
            }
        }
        for (tag, category) in &self.triage.tag_mappings {
            if tag.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "a tag_mappings entry has an empty tag".to_owned(),
                ));
            }
            if category.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "tag_mappings[{tag:?}] maps to an empty category"
                )));
            }
        }
        for account in self.triage.account_scopes.keys() {
            if account.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "an account_scopes entry has an empty account id".to_owned(),
                ));
            }
        }
        // Tier-2 tuning: a precision gate outside (0, 1] is meaningless (a gate of 0 would activate
        // any model; > 1 could never be cleared), and a zero corpus ceiling would train on nothing.
        if !(self.tier2.precision_gate > 0.0 && self.tier2.precision_gate <= 1.0) {
            return Err(ConfigError::Invalid(format!(
                "tier2.precision_gate must be in (0, 1], got {}",
                self.tier2.precision_gate
            )));
        }
        if self.tier2.max_rows == 0 {
            return Err(ConfigError::Invalid(
                "tier2.max_rows must be greater than 0".to_owned(),
            ));
        }
        // A zero batch cap is rejected loudly rather than silently coerced to the default — a
        // hand-edited `0` almost certainly means "I want to disable this", which is not what a cap
        // of 0 does (the drain bounds, never disables), so surface the mistake.
        if self.followups.drain_batch_cap == 0 {
            return Err(ConfigError::Invalid(
                "followups.drain_batch_cap must be greater than 0".to_owned(),
            ));
        }
        if self.followups.reminder_batch_cap == 0 {
            return Err(ConfigError::Invalid(
                "followups.reminder_batch_cap must be greater than 0".to_owned(),
            ));
        }
        Ok(())
    }

    /// Map this config's storage selection into a concrete [`StorageConfig`]. An absent
    /// `database_path` resolves to `<default_dir>/mailmate.db`; `"memory"` selects an
    /// in-memory database; any other value is a file path.
    #[must_use]
    pub fn storage_config(&self, default_dir: &Path) -> StorageConfig {
        match self.storage.database_path.as_deref() {
            None => StorageConfig::sqlite_file(default_dir.join("mailmate.db")),
            Some(IN_MEMORY_DB) => StorageConfig {
                engine: mailmate_ports::storage::Dialect::Sqlite,
                path: StoragePath::InMemory,
            },
            Some(path) => StorageConfig::sqlite_file(PathBuf::from(path)),
        }
    }

    /// The **effective** retention level — the configured level gated by consent. A
    /// body-retaining level is honoured only with affirmative `body_consent`; without it the
    /// effective level is clamped to `metadata`, so a body can never be stored before the user
    /// has opted in (the onboarding consent gate, enforced in one place).
    #[must_use]
    pub fn retention_level(&self) -> RetentionLevel {
        if self.retention.level.retains_body() && !self.retention.body_consent {
            RetentionLevel::Metadata
        } else {
            self.retention.level
        }
    }

    /// A read-only, secret-free snapshot of the effective settings — the payload the
    /// `get_settings` host request returns (and the `config` subcommand prints). It never
    /// includes a provider's API key (those live in the secret store, never the config).
    #[must_use]
    pub fn settings_snapshot(&self) -> SettingsSnapshot {
        SettingsSnapshot {
            // The EFFECTIVE level (consent-gated) — what is actually enforced — plus the raw
            // consent flag so the onboarding UI knows whether the consent step is still pending.
            retention_level: self.retention_level().as_str().to_owned(),
            body_consent: self.retention.body_consent,
            database: self
                .storage
                .database_path
                .clone()
                .unwrap_or_else(|| "(default data directory)".to_owned()),
            catch_up_on_launch: self.followups.catch_up_on_launch,
            follow_up_tick_seconds: self.followups.tick_seconds,
            default_provider: self.ai.default_provider.clone(),
            providers: self
                .ai
                .providers
                .iter()
                .map(|p| ProviderSummary {
                    id: p.id.clone(),
                    kind: p.kind.clone(),
                    endpoint: p.endpoint.clone(),
                    model: p.model.clone(),
                })
                .collect(),
            paused: self.paused,
            // The category vocabulary the per-message panel offers for a one-click category
            // correction — surfaced here so the extension never hardcodes the list. Starters plus
            // any tag-mapped category, so a user's own tag becomes a first-class category.
            categories: self.triage.effective_categories(),
            // The triage-tuning surface: per-category policy as wire tokens, per-account scope,
            // and the tag→category mappings — the Options page renders and edits these.
            category_policies: self
                .triage
                .category_policies
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().to_owned()))
                .collect(),
            account_scopes: self.triage.account_scopes.clone(),
            tag_mappings: self.triage.tag_mappings.clone(),
        }
    }
}

/// A secret-free view of the effective settings, returned by `get_settings`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SettingsSnapshot {
    /// The **effective** retention level (`metadata` / `bodies` / `summaries`) — the configured
    /// level after the consent gate is applied.
    pub retention_level: String,
    /// Whether the user has affirmatively consented to body storage (drives the onboarding
    /// consent step; a body-retaining level is inert until this is `true`).
    pub body_consent: bool,
    /// The database location (`"memory"`, a path, or the default-dir sentinel).
    pub database: String,
    /// Whether overdue follow-ups are drained at launch.
    pub catch_up_on_launch: bool,
    /// The periodic follow-up tick interval (`0` = startup catch-up only).
    pub follow_up_tick_seconds: u64,
    /// The provider used for LLM-always tasks, if any.
    pub default_provider: Option<String>,
    /// The configured providers (id + kind + endpoint — never keys).
    pub providers: Vec<ProviderSummary>,
    /// Whether the global pause kill-switch is engaged.
    pub paused: bool,
    /// The category vocabulary the panel offers for a one-click category correction (starters +
    /// any tag-mapped category).
    pub categories: Vec<CategoryDescriptor>,
    /// Per-category action policy as wire tokens (`auto`/`suggest`/`off`), keyed by category key.
    /// Sparse — an absent category is `auto`.
    pub category_policies: BTreeMap<String, String>,
    /// Per-account triage scope, keyed by account id. Sparse — an absent account is in scope.
    pub account_scopes: BTreeMap<String, bool>,
    /// Tag→category mappings (tag key → category key).
    pub tag_mappings: BTreeMap<String, String>,
}

/// A provider's public identity in a [`SettingsSnapshot`]. Never carries the API key (those live
/// only in the 0600 secret store); the UI shows `configured` from a separate secret-presence read.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProviderSummary {
    /// The provider id.
    pub id: String,
    /// The adapter kind.
    pub kind: String,
    /// The endpoint URL (for network adapters), if set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// The model name, if set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_document_is_the_default_config() {
        let config: AppConfig = toml::from_str("").unwrap();
        assert_eq!(config, AppConfig::default());
        assert!(
            config.followups.catch_up_on_launch,
            "catch-up is on by default"
        );
        assert_eq!(config.followups.tick_seconds, 0);
        assert_eq!(config.retention_level(), RetentionLevel::Metadata);
        assert!(config.ai.providers.is_empty(), "ships with zero providers");
        config.validate().unwrap();
    }

    #[test]
    fn body_consent_gate_clamps_the_effective_retention_to_metadata() {
        // A body-retaining level WITHOUT consent is inert: the effective level is metadata, so
        // a body can never be stored before the user opts in (the consent-declined invariant).
        let mut cfg = AppConfig::default();
        cfg.retention.level = RetentionLevel::Bodies;
        cfg.retention.body_consent = false;
        assert_eq!(
            cfg.retention_level(),
            RetentionLevel::Metadata,
            "no consent ⇒ effective level stays metadata"
        );
        assert_eq!(cfg.settings_snapshot().retention_level, "metadata");
        assert!(!cfg.settings_snapshot().body_consent);

        // With consent, the configured body-retaining level takes effect.
        cfg.retention.body_consent = true;
        assert_eq!(cfg.retention_level(), RetentionLevel::Bodies);
        assert_eq!(cfg.settings_snapshot().retention_level, "bodies");
        assert!(cfg.settings_snapshot().body_consent);

        // Consent alone never raises a metadata configuration above metadata.
        let mut meta = AppConfig::default();
        meta.retention.body_consent = true;
        assert_eq!(meta.retention_level(), RetentionLevel::Metadata);
    }

    #[test]
    fn round_trips_through_toml() {
        let config = AppConfig {
            storage: StorageSettings {
                database_path: Some("/var/lib/mailmate/db.sqlite".to_owned()),
            },
            retention: RetentionSettings {
                level: RetentionLevel::Bodies,
                body_consent: true,
            },
            followups: FollowupSettings {
                catch_up_on_launch: true,
                tick_seconds: 300,
                ..Default::default()
            },
            ai: AiSettings {
                default_provider: Some("local-ollama".to_owned()),
                providers: vec![ProviderSettings {
                    id: "local-ollama".to_owned(),
                    kind: "ollama".to_owned(),
                    endpoint: Some("http://127.0.0.1:11434".to_owned()),
                    model: Some("llama3".to_owned()),
                }],
            },
            triage: TriageSettings {
                category_policies: BTreeMap::from([
                    ("newsletters".to_owned(), CategoryPolicy::Off),
                    ("receipts".to_owned(), CategoryPolicy::Suggest),
                ]),
                account_scopes: BTreeMap::from([("acct_archive".to_owned(), false)]),
                tag_mappings: BTreeMap::from([("$label1".to_owned(), "work".to_owned())]),
            },
            tier2: Tier2Settings {
                max_rows: 5_000,
                precision_gate: 0.9,
            },
            paused: false,
        };
        let text = config.to_toml().unwrap();
        let back: AppConfig = toml::from_str(&text).unwrap();
        assert_eq!(back, config);
        back.validate().unwrap();
    }

    #[test]
    fn tier2_and_followup_knobs_parse_from_toml_and_validate() {
        let toml = r#"
            [followups]
            drain_batch_cap = 25
            reminder_batch_cap = 10

            [tier2]
            max_rows = 5000
            precision_gate = 0.95
        "#;
        let cfg: AppConfig = toml::from_str(toml).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.followups.drain_batch_cap, 25);
        assert_eq!(cfg.followups.reminder_batch_cap, 10);
        assert_eq!(cfg.tier2.max_rows, 5000);
        assert!((cfg.tier2.precision_gate - 0.95).abs() < 1e-9);
    }

    #[test]
    fn tier2_defaults_mirror_the_training_service_constants() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.tier2.max_rows, 20_000);
        assert!((cfg.tier2.precision_gate - 0.8).abs() < 1e-9);
    }

    #[test]
    fn an_out_of_range_precision_gate_is_rejected() {
        let mut cfg = AppConfig::default();
        cfg.tier2.precision_gate = 1.5;
        assert!(cfg.validate().is_err(), "a gate > 1 can never be cleared");
        cfg.tier2.precision_gate = 0.0;
        assert!(
            cfg.validate().is_err(),
            "a gate of 0 would activate any model"
        );
        cfg.tier2.precision_gate = 0.8;
        cfg.tier2.max_rows = 0;
        assert!(
            cfg.validate().is_err(),
            "a zero corpus ceiling trains on nothing"
        );
    }

    #[test]
    fn a_zero_batch_cap_is_rejected_not_silently_coerced() {
        let mut cfg = AppConfig::default();
        cfg.followups.drain_batch_cap = 0;
        assert!(
            cfg.validate().is_err(),
            "a zero drain cap is a config mistake, not 'disable'"
        );
        cfg.followups.drain_batch_cap = 100;
        cfg.followups.reminder_batch_cap = 0;
        assert!(
            cfg.validate().is_err(),
            "a zero reminder cap is rejected too"
        );
    }

    #[test]
    fn triage_defaults_are_empty_and_sparse_lookups_return_defaults() {
        let cfg = AppConfig::default();
        assert!(cfg.triage.category_policies.is_empty());
        assert!(cfg.triage.account_scopes.is_empty());
        assert!(cfg.triage.tag_mappings.is_empty());
        // Absent ⇒ default posture: Auto for any category, in-scope for any account.
        assert_eq!(cfg.triage.policy_for("newsletters"), CategoryPolicy::Auto);
        assert!(cfg.triage.account_in_scope("acct_whatever"));
    }

    #[test]
    fn category_policy_restrictiveness_orders_off_over_suggest_over_auto() {
        assert_eq!(
            CategoryPolicy::Auto.most_restrictive(CategoryPolicy::Suggest),
            CategoryPolicy::Suggest
        );
        assert_eq!(
            CategoryPolicy::Suggest.most_restrictive(CategoryPolicy::Off),
            CategoryPolicy::Off
        );
        assert_eq!(
            CategoryPolicy::Off.most_restrictive(CategoryPolicy::Auto),
            CategoryPolicy::Off
        );
        // Tokens round-trip through parse/as_str.
        for p in [
            CategoryPolicy::Auto,
            CategoryPolicy::Suggest,
            CategoryPolicy::Off,
        ] {
            assert_eq!(CategoryPolicy::parse(p.as_str()), Some(p));
        }
        assert_eq!(CategoryPolicy::parse("nonsense"), None);
    }

    #[test]
    fn a_tag_mapping_to_a_new_category_surfaces_it_in_the_vocabulary() {
        let mut cfg = AppConfig::default();
        // A tag mapped to an EXISTING category does not duplicate it…
        cfg.triage
            .tag_mappings
            .insert("$label1".to_owned(), "work".to_owned());
        // …but a tag mapped to a NEW category key surfaces it as a first-class category.
        cfg.triage
            .tag_mappings
            .insert("$label2".to_owned(), "vip".to_owned());

        let cats = cfg.triage.effective_categories();
        let work_count = cats.iter().filter(|c| c.key == "work").count();
        assert_eq!(work_count, 1, "an existing category is not duplicated");
        let vip = cats.iter().find(|c| c.key == "vip").expect("derived vip");
        assert_eq!(vip.label, "Vip", "a derived category is humanized");
        assert!(cfg.triage.is_known_category("vip"));
        assert!(cfg.triage.is_known_category("newsletters"));
        assert!(!cfg.triage.is_known_category("not-a-category"));
    }

    #[test]
    fn validate_rejects_empty_triage_keys_and_values() {
        // A hand-edited config bypasses the router guards, so validate() must catch empties.
        let mut empty_cat = AppConfig::default();
        empty_cat
            .triage
            .category_policies
            .insert("   ".to_owned(), CategoryPolicy::Off);
        assert!(empty_cat.validate().is_err(), "empty category key rejected");

        let mut empty_val = AppConfig::default();
        empty_val
            .triage
            .tag_mappings
            .insert("$label1".to_owned(), "".to_owned());
        assert!(empty_val.validate().is_err(), "empty tag category rejected");

        let mut empty_tag = AppConfig::default();
        empty_tag
            .triage
            .tag_mappings
            .insert("  ".to_owned(), "work".to_owned());
        assert!(empty_tag.validate().is_err(), "empty tag key rejected");

        // A well-formed triage config validates.
        let mut ok = AppConfig::default();
        ok.triage
            .category_policies
            .insert("newsletters".to_owned(), CategoryPolicy::Off);
        ok.triage
            .tag_mappings
            .insert("$label1".to_owned(), "work".to_owned());
        ok.validate().unwrap();
    }

    #[test]
    fn pruning_drops_a_policy_whose_tag_derived_category_is_gone() {
        let mut cfg = AppConfig::default();
        // A tag introduces category "vip"; the user sets a policy on it.
        cfg.triage
            .tag_mappings
            .insert("$label1".to_owned(), "vip".to_owned());
        cfg.triage
            .category_policies
            .insert("vip".to_owned(), CategoryPolicy::Off);
        // A starter-category policy must survive pruning regardless of tags.
        cfg.triage
            .category_policies
            .insert("newsletters".to_owned(), CategoryPolicy::Suggest);

        // Nothing to prune yet (vip is still derived from the live mapping).
        assert!(!cfg.triage.prune_orphaned_category_policies());

        // Remove the mapping → vip is no longer a known category → its policy is orphaned.
        cfg.triage.tag_mappings.remove("$label1");
        assert!(cfg.triage.prune_orphaned_category_policies(), "pruned vip");
        assert!(
            !cfg.triage.category_policies.contains_key("vip"),
            "the orphaned policy is gone, so it cannot silently reactivate"
        );
        assert_eq!(
            cfg.triage.category_policies.get("newsletters"),
            Some(&CategoryPolicy::Suggest),
            "a starter-category policy is untouched"
        );
    }

    #[test]
    fn the_snapshot_carries_the_triage_surface() {
        let mut cfg = AppConfig::default();
        cfg.triage
            .category_policies
            .insert("promotions".to_owned(), CategoryPolicy::Off);
        cfg.triage
            .account_scopes
            .insert("acct_old".to_owned(), false);
        cfg.triage
            .tag_mappings
            .insert("$label3".to_owned(), "leads".to_owned());

        let snap = cfg.settings_snapshot();
        assert_eq!(
            snap.category_policies.get("promotions").map(String::as_str),
            Some("off"),
            "policy serializes as a wire token"
        );
        assert_eq!(snap.account_scopes.get("acct_old"), Some(&false));
        assert_eq!(
            snap.tag_mappings.get("$label3").map(String::as_str),
            Some("leads")
        );
        // The tag-derived category is in the surfaced vocabulary.
        assert!(snap.categories.iter().any(|c| c.key == "leads"));
    }

    #[test]
    fn storage_config_maps_the_three_path_forms() {
        let dir = Path::new("/data/dir");

        let default_dir_cfg = AppConfig::default().storage_config(dir);
        assert_eq!(
            default_dir_cfg.path,
            StoragePath::File(dir.join("mailmate.db"))
        );

        let mut mem = AppConfig::default();
        mem.storage.database_path = Some("memory".to_owned());
        assert_eq!(mem.storage_config(dir).path, StoragePath::InMemory);

        let mut file = AppConfig::default();
        file.storage.database_path = Some("/custom/x.db".to_owned());
        assert_eq!(
            file.storage_config(dir).path,
            StoragePath::File(PathBuf::from("/custom/x.db"))
        );
    }

    #[test]
    fn validate_rejects_an_unknown_provider_kind() {
        let mut config = AppConfig::default();
        config.ai.providers.push(ProviderSettings {
            id: "p".to_owned(),
            kind: "not_a_real_backend".to_owned(),
            endpoint: None,
            model: None,
        });
        assert!(matches!(config.validate(), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn validate_rejects_a_dangling_default_provider_and_duplicate_ids() {
        let mut dangling = AppConfig::default();
        dangling.ai.default_provider = Some("ghost".to_owned());
        assert!(matches!(dangling.validate(), Err(ConfigError::Invalid(_))));

        let mut dup = AppConfig::default();
        dup.ai.providers = vec![
            ProviderSettings {
                id: "p".to_owned(),
                kind: "mock".to_owned(),
                endpoint: None,
                model: None,
            },
            ProviderSettings {
                id: "p".to_owned(),
                kind: "mock".to_owned(),
                endpoint: None,
                model: None,
            },
        ];
        assert!(matches!(dup.validate(), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn load_and_save_round_trip_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        let config = AppConfig {
            retention: RetentionSettings {
                level: RetentionLevel::Summaries,
                body_consent: true,
            },
            ..AppConfig::default()
        };
        config.save(&path).unwrap();
        let loaded = AppConfig::load(&path).unwrap();
        assert_eq!(loaded.retention_level(), RetentionLevel::Summaries);
    }

    #[test]
    fn load_reports_a_parse_error_on_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "this is = = not toml").unwrap();
        assert!(matches!(AppConfig::load(&path), Err(ConfigError::Parse(_))));
    }

    #[test]
    fn load_reports_an_io_error_for_a_missing_file() {
        let err = AppConfig::load(Path::new("/no/such/config.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }));
    }
}
