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

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
    /// The active retention level (default `metadata`: no readable body stored).
    pub level: RetentionLevel,
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
}

impl Default for FollowupSettings {
    fn default() -> Self {
        Self {
            catch_up_on_launch: true,
            tick_seconds: 0,
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

    /// The active retention level.
    #[must_use]
    pub fn retention_level(&self) -> RetentionLevel {
        self.retention.level
    }

    /// A read-only, secret-free snapshot of the effective settings — the payload the
    /// `get_settings` host request returns (and the `config` subcommand prints). It never
    /// includes a provider's API key (those live in the secret store, never the config).
    #[must_use]
    pub fn settings_snapshot(&self) -> SettingsSnapshot {
        SettingsSnapshot {
            retention_level: self.retention.level.as_str().to_owned(),
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
                })
                .collect(),
            paused: self.paused,
        }
    }
}

/// A secret-free view of the effective settings, returned by `get_settings`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SettingsSnapshot {
    /// The active retention level (`metadata` / `bodies` / `summaries`).
    pub retention_level: String,
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
    fn round_trips_through_toml() {
        let config = AppConfig {
            storage: StorageSettings {
                database_path: Some("/var/lib/mailmate/db.sqlite".to_owned()),
            },
            retention: RetentionSettings {
                level: RetentionLevel::Bodies,
            },
            followups: FollowupSettings {
                catch_up_on_launch: true,
                tick_seconds: 300,
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
            paused: false,
        };
        let text = config.to_toml().unwrap();
        let back: AppConfig = toml::from_str(&text).unwrap();
        assert_eq!(back, config);
        back.validate().unwrap();
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
