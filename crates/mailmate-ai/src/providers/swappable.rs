//! A hot-swappable [`AiProvider`] wrapper.
//!
//! The composition root builds the live provider once at startup from `[ai]` config. But a user
//! who adds a provider or saves an API key from Settings *after* the host has started would
//! otherwise keep drafting through the old (often [`UnavailableProvider`]) adapter until a
//! restart. Wrapping the live provider in a [`SwappableProvider`] — injected into every
//! provider-typed collaborator (the reply drafter, the curator, the training evaluator) — lets
//! the host rebuild and swap the backing adapter in place the moment those settings change, with
//! no restart. This mirrors the `SwappableTier2` / rule-engine hot-reload shape already used
//! elsewhere: reads take a snapshot `Arc` and release the lock before awaiting, so a swap never
//! blocks an in-flight completion.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use mailmate_common::ai::{
    ProviderCapabilities, ProviderId, StructuredRequest, StructuredResponse,
};
use mailmate_common::error::AiError;
use mailmate_ports::ai_provider::AiProvider;

/// An [`AiProvider`] whose backing adapter can be hot-swapped in place behind its `Arc`. Every
/// provider-typed collaborator holds the SAME `Arc<SwappableProvider>`, so one [`swap`](Self::swap)
/// updates what all of them use for the next completion.
pub struct SwappableProvider {
    current: RwLock<Arc<dyn AiProvider>>,
}

impl SwappableProvider {
    /// Wrap an initial backing provider (the one built from startup config — often the configured
    /// adapter, or [`UnavailableProvider`](super::UnavailableProvider) at zero-provider cold start).
    #[must_use]
    pub fn new(initial: Arc<dyn AiProvider>) -> Self {
        Self {
            current: RwLock::new(initial),
        }
    }

    /// Swap in a freshly-built backing provider; subsequent completions use it.
    pub fn swap(&self, next: Arc<dyn AiProvider>) {
        *self
            .current
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = next;
    }

    /// A cloned snapshot of the current backing provider — taken under the read lock, which is
    /// then released before the caller awaits on it.
    fn snapshot(&self) -> Arc<dyn AiProvider> {
        self.current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl AiProvider for SwappableProvider {
    fn id(&self) -> ProviderId {
        self.snapshot().id()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.snapshot().capabilities()
    }

    async fn complete_structured(
        &self,
        request: StructuredRequest,
    ) -> Result<StructuredResponse, AiError> {
        self.snapshot().complete_structured(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    use crate::providers::{MockProvider, UnavailableProvider};

    fn req() -> StructuredRequest {
        use mailmate_common::ai::SamplingParams;
        StructuredRequest {
            messages: vec![],
            json_schema: None,
            grammar: None,
            sampling: SamplingParams::default(),
        }
    }

    #[test]
    fn delegates_to_the_current_backing_provider() {
        let swap = SwappableProvider::new(Arc::new(UnavailableProvider::new()));
        // Cold start: an unavailable backing degrades, never fabricates.
        assert_eq!(swap.id(), ProviderId::from("unavailable"));
        assert!(matches!(
            block_on(swap.complete_structured(req())),
            Err(AiError::Unavailable(_))
        ));
    }

    #[test]
    fn a_swap_changes_what_subsequent_completions_use() {
        let swap = SwappableProvider::new(Arc::new(UnavailableProvider::new()));
        // Simulate the user configuring a provider from Settings after startup.
        swap.swap(Arc::new(MockProvider::returning_json(
            "configured",
            serde_json::json!({ "ok": true }),
        )));
        assert_eq!(swap.id(), ProviderId::from("configured"));
        let resp = block_on(swap.complete_structured(req())).expect("now configured");
        assert_eq!(resp.parsed_json, serde_json::json!({ "ok": true }));
    }
}
