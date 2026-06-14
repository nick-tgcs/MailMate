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

use mailmate_common::ai::{
    ProviderCapabilities, ProviderId, StructuredRequest, StructuredResponse,
};
use mailmate_common::error::AiError;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn AiProvider) {}
        let _ = takes as fn(&dyn AiProvider);
    }
}
