//! The LM Studio adapter: OpenAI-shaped chat completions with a compiled GBNF grammar for
//! guaranteed-valid JSON. Endpoint and model are adapter-private.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use mailmate_common::ai::{
    ProviderCapabilities, ProviderId, StructuredRequest, StructuredResponse,
};
use mailmate_common::error::AiError;
use mailmate_ports::ai_provider::AiProvider;

use crate::http::HttpClient;
use crate::providers::{schema_to_gbnf, structured_from_content};

/// An adapter for a local LM Studio server.
pub struct LmStudioAdapter {
    id: ProviderId,
    endpoint: String,
    model: String,
    http: Arc<dyn HttpClient>,
}

impl LmStudioAdapter {
    /// Build the adapter. `endpoint` is the base (e.g. `http://localhost:1234/v1`).
    #[must_use]
    pub fn new(
        id: impl Into<ProviderId>,
        endpoint: impl Into<String>,
        model: impl Into<String>,
        http: Arc<dyn HttpClient>,
    ) -> Self {
        Self {
            id: id.into(),
            endpoint: endpoint.into(),
            model: model.into(),
            http,
        }
    }

    fn body(&self, request: &StructuredRequest) -> Value {
        json!({
            "model": self.model,
            "messages": request.messages,
            "temperature": request.sampling.temperature,
            // LM Studio accepts a GBNF grammar to guarantee valid JSON output.
            "grammar": schema_to_gbnf(request.json_schema.as_ref()),
        })
    }
}

#[async_trait]
impl AiProvider for LmStudioAdapter {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        // Honest capabilities: this adapter enforces structure via a GBNF grammar, NOT the
        // native `response_format: json_schema` path — so it advertises grammar, not schema.
        ProviderCapabilities {
            grammar: true,
            json_schema: false,
            function_calling: false,
            max_context_tokens: None,
        }
    }

    async fn complete_structured(
        &self,
        request: StructuredRequest,
    ) -> Result<StructuredResponse, AiError> {
        let url = format!("{}/chat/completions", self.endpoint);
        let response = self
            .http
            .post_json(
                &url,
                vec![("content-type".to_owned(), "application/json".to_owned())],
                self.body(&request),
            )
            .await?;
        let content = response
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AiError::Validation("response missing choices[0].message.content".to_owned())
            })?;
        structured_from_content(content, "gbnf_grammar")
    }
}
