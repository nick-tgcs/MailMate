//! The AI-provider port: the single structured-completion primitive.
//!
//! A provider does exactly one thing — take a prompt + an optional schema/grammar and
//! return a validated structured response. All task semantics (classify, draft,
//! summarize, extract, propose) live ABOVE this in `mailmate-ai::tasks` as typed functions,
//! so a new task is one function with zero adapter changes. The generative foundation model
//! always runs **frozen**: there is no fine-tuning or weight-update method on this port.
//!
//! Per the universal ports law the core names only this trait; concrete adapters (mock,
//! Ollama, OpenAI-compatible, LM Studio, llama.cpp, feature-gated Burn) live in
//! `mailmate-ai`, and no provider-specific detail crosses this boundary.

use async_trait::async_trait;

use mailmate_common::adapter::{AdapterCompatibility, AdapterSpec};
use mailmate_common::ai::{
    ProviderCapabilities, ProviderId, StructuredRequest, StructuredResponse,
};
use mailmate_common::error::AiError;
use mailmate_common::ids::AdapterId;

/// A configured AI provider.
#[async_trait]
pub trait AiProvider: Send + Sync {
    /// This provider's configured identity.
    fn id(&self) -> ProviderId;

    /// What structured-output enforcement this provider supports (advisory).
    fn capabilities(&self) -> ProviderCapabilities;

    /// Complete one structured request, returning the validated structured response.
    ///
    /// # Errors
    /// [`AiError`] on transport failure, schema-enforcement failure, or a response that
    /// cannot be parsed.
    async fn complete_structured(
        &self,
        request: StructuredRequest,
    ) -> Result<StructuredResponse, AiError>;
}

/// An **optional** provider capability: loading a portable adapter (e.g. a LoRA) onto the
/// base model. Most providers do not implement this — a LoRA-adapted provider is still just
/// a configured `AiProvider` whose output flows through the same validation → rules → policy
/// path, so loading an adapter never grants it a way around those gates. A provider
/// advertises compatibility via [`can_load_adapter`](SupportsAdapters::can_load_adapter)
/// rather than assuming any backend; a hard-incompatible adapter must be refused.
#[async_trait]
pub trait SupportsAdapters: AiProvider {
    /// Whether this provider can load `adapter`, as a metadata-only compatibility verdict.
    fn can_load_adapter(&self, adapter: &AdapterSpec) -> AdapterCompatibility;

    /// Load `adapter` so subsequent completions are adapter-influenced.
    ///
    /// # Errors
    /// [`AiError`] if the adapter is incompatible or cannot be loaded.
    async fn load_adapter(&self, adapter: AdapterSpec) -> Result<(), AiError>;

    /// Unload a previously-loaded adapter, reverting to the frozen base model.
    ///
    /// # Errors
    /// [`AiError`] if no such adapter is loaded or it cannot be unloaded.
    async fn unload_adapter(&self, adapter_id: AdapterId) -> Result<(), AiError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn AiProvider) {}
        let _ = takes as fn(&dyn AiProvider);
    }

    #[test]
    fn adapter_capability_is_object_safe() {
        fn takes(_: &dyn SupportsAdapters) {}
        let _ = takes as fn(&dyn SupportsAdapters);
    }
}
