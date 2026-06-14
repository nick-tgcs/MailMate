//! The deterministic mock provider — the test/CI provider that needs no network and no
//! external model. It returns a programmed [`StructuredResponse`] (or error), so every
//! task and the validation layer can be tested deterministically.

use async_trait::async_trait;

use mailmate_common::ai::{
    ProviderCapabilities, ProviderId, StructuredRequest, StructuredResponse,
};
use mailmate_common::error::AiError;
use mailmate_ports::ai_provider::AiProvider;

/// A provider that returns a fixed, programmed outcome.
#[derive(Debug)]
pub struct MockProvider {
    id: ProviderId,
    outcome: Outcome,
}

#[derive(Debug)]
enum Outcome {
    Respond(Box<StructuredResponse>),
    Fail(AiError),
}

impl MockProvider {
    /// A mock that always returns `response`.
    #[must_use]
    pub fn returning(id: impl Into<ProviderId>, response: StructuredResponse) -> Self {
        Self {
            id: id.into(),
            outcome: Outcome::Respond(Box::new(response)),
        }
    }

    /// A mock that always returns `value` as a parsed JSON response.
    #[must_use]
    pub fn returning_json(id: impl Into<ProviderId>, value: serde_json::Value) -> Self {
        Self::returning(
            id,
            StructuredResponse {
                raw_text: value.to_string(),
                parsed_json: value,
                schema_validated_by: Some("mock".to_owned()),
            },
        )
    }

    /// A mock that always fails with `error`.
    #[must_use]
    pub fn failing(id: impl Into<ProviderId>, error: AiError) -> Self {
        Self {
            id: id.into(),
            outcome: Outcome::Fail(error),
        }
    }
}

#[async_trait]
impl AiProvider for MockProvider {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            grammar: false,
            json_schema: true,
            function_calling: false,
            max_context_tokens: None,
        }
    }

    async fn complete_structured(
        &self,
        _request: StructuredRequest,
    ) -> Result<StructuredResponse, AiError> {
        match &self.outcome {
            Outcome::Respond(response) => Ok((**response).clone()),
            Outcome::Fail(error) => Err(error.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use mailmate_common::ai::SamplingParams;
    use serde_json::json;

    fn req() -> StructuredRequest {
        StructuredRequest {
            messages: vec![],
            json_schema: None,
            grammar: None,
            sampling: SamplingParams::default(),
        }
    }

    #[test]
    fn returns_programmed_json() {
        let mock = MockProvider::returning_json("mock", json!({ "ok": 1 }));
        let resp = block_on(mock.complete_structured(req())).unwrap();
        assert_eq!(resp.parsed_json, json!({ "ok": 1 }));
        assert_eq!(mock.id(), ProviderId::from("mock"));
    }

    #[test]
    fn returns_programmed_error() {
        let mock = MockProvider::failing("mock", AiError::Unavailable("no model".to_owned()));
        let err = block_on(mock.complete_structured(req())).unwrap_err();
        assert!(matches!(err, AiError::Unavailable(_)));
    }
}
