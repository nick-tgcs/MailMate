//! The "no provider configured" stand-in.
//!
//! The provider registry ships **empty** by design: MailMate is fully functional with zero
//! providers (Tier 1/2 classification, the rule/policy/audit spine, the learning loop, and
//! review-required draft *slots* all run with no LLM). But several collaborators are typed
//! over a non-optional `Arc<dyn AiProvider>` (the reply drafter, the curator, the training
//! pipeline's evaluator). Injecting [`UnavailableProvider`] there is the honest zero-provider
//! wiring: every structured completion fails with [`AiError::Unavailable`], so an
//! LLM-always task **degrades** (a Tier-3-needed message → review; a draft request → an
//! error the host surfaces) rather than silently fabricating output. It never returns a
//! response, so no fake classification or draft can originate here.

use async_trait::async_trait;

use mailmate_common::ai::{
    ProviderCapabilities, ProviderId, StructuredRequest, StructuredResponse,
};
use mailmate_common::error::AiError;
use mailmate_ports::ai_provider::AiProvider;

/// The provider id [`UnavailableProvider`] reports.
pub const UNAVAILABLE_PROVIDER_ID: &str = "unavailable";

/// A provider that is always unavailable — the stand-in when no real provider is configured.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableProvider;

impl UnavailableProvider {
    /// Construct the stand-in.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AiProvider for UnavailableProvider {
    fn id(&self) -> ProviderId {
        ProviderId::from(UNAVAILABLE_PROVIDER_ID)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        // Advertises nothing — there is no backend to enforce anything.
        ProviderCapabilities {
            grammar: false,
            json_schema: false,
            function_calling: false,
            max_context_tokens: None,
        }
    }

    async fn complete_structured(
        &self,
        _request: StructuredRequest,
    ) -> Result<StructuredResponse, AiError> {
        Err(AiError::Unavailable(
            "no AI provider is configured; configure one in [ai] to enable LLM features".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use mailmate_common::ai::SamplingParams;

    fn req() -> StructuredRequest {
        StructuredRequest {
            messages: vec![],
            json_schema: None,
            grammar: None,
            sampling: SamplingParams::default(),
        }
    }

    #[test]
    fn reports_a_stable_id_and_no_capabilities() {
        let provider = UnavailableProvider::new();
        assert_eq!(provider.id(), ProviderId::from("unavailable"));
        let caps = provider.capabilities();
        assert!(!caps.grammar && !caps.json_schema && !caps.function_calling);
    }

    #[test]
    fn every_completion_is_unavailable_never_a_fabricated_response() {
        let err = block_on(UnavailableProvider::new().complete_structured(req())).unwrap_err();
        assert!(matches!(err, AiError::Unavailable(_)), "got {err:?}");
    }
}
