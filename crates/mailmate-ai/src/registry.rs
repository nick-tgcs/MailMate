//! The provider registry: holds the configured providers, keyed by [`ProviderId`].
//!
//! Ships **empty** — `default_provider` is a user choice, never a shipped value. An absent
//! provider is configured-by-omission, not a system error; LLM-always tasks degrade to
//! review rather than crashing.

use std::collections::HashMap;

use mailmate_common::ai::ProviderId;
use mailmate_ports::ai_provider::AiProvider;

/// A registry of configured AI providers.
#[derive(Default)]
pub struct ProviderRegistry {
    providers: HashMap<ProviderId, Box<dyn AiProvider>>,
}

impl ProviderRegistry {
    /// A new, empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider under its own id (overwriting any prior registration).
    pub fn register(&mut self, provider: Box<dyn AiProvider>) {
        self.providers.insert(provider.id(), provider);
    }

    /// Borrow a provider by id, or `None` if none is registered.
    #[must_use]
    pub fn get(&self, id: &ProviderId) -> Option<&dyn AiProvider> {
        self.providers.get(id).map(Box::as_ref)
    }

    /// Whether the registry has no providers.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// How many providers are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.providers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::mock::MockProvider;
    use serde_json::json;

    #[test]
    fn ships_empty_and_round_trips_registrations() {
        let mut registry = ProviderRegistry::new();
        assert!(registry.is_empty(), "the registry ships empty");
        assert!(registry.get(&ProviderId::from("mock")).is_none());

        registry.register(Box::new(MockProvider::returning_json(
            "mock",
            json!({ "ok": true }),
        )));
        assert_eq!(registry.len(), 1);
        let got = registry.get(&ProviderId::from("mock")).unwrap();
        assert_eq!(got.id(), ProviderId::from("mock"));
        assert!(registry.get(&ProviderId::from("nope")).is_none());
    }
}
