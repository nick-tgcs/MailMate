//! Build the live `AiProvider` from configuration — the edge wiring the architecture promises.
//!
//! mailmate-ai ships its provider registry empty; the host is where a *configured* provider is
//! constructed and injected. [`build_provider`] reads the `[ai]` settings, picks the default
//! provider, and builds the matching adapter over the host's local HTTP transport. It DEGRADES
//! to [`UnavailableProvider`] (never panics) when no provider is configured, the kind is unknown,
//! or a required field (endpoint / model) is missing — so the host always stands up, and an
//! LLM-always task honestly reports "needs a provider" instead of fabricating output.

use std::sync::Arc;

use futures::executor::block_on;

use mailmate_ai::http::HttpClient;
use mailmate_ai::providers::{
    LlamaCppAdapter, LmStudioAdapter, OllamaAdapter, OpenAiCompatibleAdapter, UnavailableProvider,
};
use mailmate_common::secret::SecretKey;
use mailmate_ports::ai_provider::AiProvider;
use mailmate_ports::secret_store::SecretStore;

use crate::config::{AiSettings, ProviderSettings};

/// The secret-store key under which a provider's API key lives (mirrors `AdminSuite::secret_key`).
fn api_key_key(provider_id: &str) -> SecretKey {
    SecretKey::from(format!("{provider_id}_api_key").as_str())
}

/// Construct the configured default provider, or [`UnavailableProvider`] when none is configured
/// or the configuration is incomplete. Never panics — every failure path degrades.
#[must_use]
pub fn build_provider(
    ai: &AiSettings,
    secrets: &dyn SecretStore,
    http: Arc<dyn HttpClient>,
) -> Arc<dyn AiProvider> {
    ai.default_provider
        .as_deref()
        .and_then(|id| ai.providers.iter().find(|p| p.id == id))
        .and_then(|settings| build_one(settings, secrets, http))
        .unwrap_or_else(|| Arc::new(UnavailableProvider::new()))
}

/// Whether the configured default provider resolves to a real adapter (vs degrading to the
/// zero-provider sentinel). This is the structural truth `provider_status` reports as `available`:
/// it runs the SAME construction predicate as [`build_provider`] (down to `build_one`), so it can
/// never drift from what the draft path would actually get — and unlike an id-string match, a
/// provider a user happens to name `"unavailable"` cannot fool it.
#[must_use]
pub fn provider_is_configured(
    ai: &AiSettings,
    secrets: &dyn SecretStore,
    http: Arc<dyn HttpClient>,
) -> bool {
    ai.default_provider
        .as_deref()
        .and_then(|id| ai.providers.iter().find(|p| p.id == id))
        .and_then(|settings| build_one(settings, secrets, http))
        .is_some()
}

/// Build one adapter from its settings, or `None` if the kind is unknown or a required field is
/// missing (endpoint for every network adapter; model for the chat adapters).
fn build_one(
    p: &ProviderSettings,
    secrets: &dyn SecretStore,
    http: Arc<dyn HttpClient>,
) -> Option<Arc<dyn AiProvider>> {
    let endpoint = p.endpoint.as_deref()?;
    match p.kind.as_str() {
        "ollama" => Some(Arc::new(OllamaAdapter::new(
            p.id.clone(),
            endpoint,
            p.model.as_deref()?,
            http,
        ))),
        "lm_studio" => Some(Arc::new(LmStudioAdapter::new(
            p.id.clone(),
            endpoint,
            p.model.as_deref()?,
            http,
        ))),
        "llama_cpp" => Some(Arc::new(LlamaCppAdapter::new(p.id.clone(), endpoint, http))),
        "openai_compatible" => {
            // The API key is optional (a local OpenAI-compatible server may need none); a store
            // read error is treated as "no key", so a transient store hiccup degrades, not panics.
            let key = block_on(secrets.get(api_key_key(&p.id))).ok().flatten();
            Some(Arc::new(OpenAiCompatibleAdapter::new(
                p.id.clone(),
                endpoint,
                p.model.as_deref()?,
                key,
                http,
            )))
        }
        // "mock" and any unknown kind have no real network home → degrade to unavailable.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::ai::ProviderId;
    use mailmate_test_support::fakes::FakeSecretStore;

    use crate::http_client::StdHttpClient;

    fn ai(default: Option<&str>, providers: Vec<ProviderSettings>) -> AiSettings {
        AiSettings {
            default_provider: default.map(str::to_owned),
            providers,
        }
    }

    fn provider(
        id: &str,
        kind: &str,
        endpoint: Option<&str>,
        model: Option<&str>,
    ) -> ProviderSettings {
        ProviderSettings {
            id: id.to_owned(),
            kind: kind.to_owned(),
            endpoint: endpoint.map(str::to_owned),
            model: model.map(str::to_owned),
        }
    }

    fn http() -> Arc<dyn HttpClient> {
        Arc::new(StdHttpClient::new())
    }

    fn built_id(cfg: &AiSettings) -> ProviderId {
        build_provider(cfg, &FakeSecretStore::new(), http()).id()
    }

    #[test]
    fn a_complete_ollama_config_builds_that_adapter() {
        let cfg = ai(
            Some("local"),
            vec![provider(
                "local",
                "ollama",
                Some("http://localhost:11434"),
                Some("llama3"),
            )],
        );
        assert_eq!(built_id(&cfg), ProviderId::from("local"));
    }

    #[test]
    fn no_default_provider_degrades_to_unavailable() {
        let cfg = ai(
            None,
            vec![provider("local", "ollama", Some("http://x"), Some("m"))],
        );
        assert_eq!(built_id(&cfg), ProviderId::from("unavailable"));
    }

    #[test]
    fn a_chat_adapter_without_a_model_degrades() {
        let cfg = ai(
            Some("local"),
            vec![provider("local", "ollama", Some("http://x"), None)],
        );
        assert_eq!(built_id(&cfg), ProviderId::from("unavailable"));
    }

    #[test]
    fn a_network_adapter_without_an_endpoint_degrades() {
        let cfg = ai(
            Some("local"),
            vec![provider("local", "ollama", None, Some("m"))],
        );
        assert_eq!(built_id(&cfg), ProviderId::from("unavailable"));
    }

    #[test]
    fn an_unknown_kind_degrades() {
        let cfg = ai(
            Some("x"),
            vec![provider("x", "telepathy", Some("http://x"), Some("m"))],
        );
        assert_eq!(built_id(&cfg), ProviderId::from("unavailable"));
    }

    #[test]
    fn an_openai_compatible_config_builds_even_without_a_key() {
        let cfg = ai(
            Some("vllm"),
            vec![provider(
                "vllm",
                "openai_compatible",
                Some("http://localhost:8000/v1"),
                Some("mixtral"),
            )],
        );
        assert_eq!(built_id(&cfg), ProviderId::from("vllm"));
    }

    #[test]
    fn llama_cpp_needs_no_model() {
        let cfg = ai(
            Some("lc"),
            vec![provider(
                "lc",
                "llama_cpp",
                Some("http://localhost:8080"),
                None,
            )],
        );
        assert_eq!(built_id(&cfg), ProviderId::from("lc"));
    }

    fn configured(cfg: &AiSettings) -> bool {
        provider_is_configured(cfg, &FakeSecretStore::new(), http())
    }

    #[test]
    fn provider_is_configured_tracks_build_provider() {
        // Complete default → configured; incomplete / no-default → not.
        assert!(configured(&ai(
            Some("local"),
            vec![provider("local", "ollama", Some("http://x"), Some("m"))],
        )));
        assert!(!configured(&ai(
            Some("local"),
            vec![provider("local", "ollama", Some("http://x"), None)], // no model
        )));
        assert!(!configured(&ai(
            None,
            vec![provider("local", "ollama", Some("http://x"), Some("m"))],
        )));
    }

    #[test]
    fn a_complete_provider_named_unavailable_is_still_configured() {
        // The structural check must not be fooled by a provider that happens to share the
        // zero-provider sentinel's id — the old id-string match reported this as unavailable.
        let cfg = ai(
            Some("unavailable"),
            vec![provider("unavailable", "ollama", Some("http://x"), Some("m"))],
        );
        assert!(configured(&cfg), "a real adapter named 'unavailable' is available");
    }
}
