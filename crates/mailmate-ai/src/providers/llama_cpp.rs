//! The llama.cpp server adapter: the native `/completion` endpoint with a GBNF grammar.
//! Endpoint and model are adapter-private.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use mailmate_common::ai::{
    ProviderCapabilities, ProviderId, StructuredRequest, StructuredResponse,
};
use mailmate_common::error::AiError;
use mailmate_ports::ai_provider::AiProvider;

use crate::http::HttpClient;
use crate::providers::{render_prompt, schema_to_gbnf, structured_from_content};

/// An adapter for a local llama.cpp server.
pub struct LlamaCppAdapter {
    id: ProviderId,
    endpoint: String,
    http: Arc<dyn HttpClient>,
}

impl LlamaCppAdapter {
    /// Build the adapter. `endpoint` is the base (e.g. `http://localhost:8080`).
    #[must_use]
    pub fn new(
        id: impl Into<ProviderId>,
        endpoint: impl Into<String>,
        http: Arc<dyn HttpClient>,
    ) -> Self {
        Self {
            id: id.into(),
            endpoint: endpoint.into(),
            http,
        }
    }

    fn body(&self, request: &StructuredRequest) -> Value {
        json!({
            "prompt": render_prompt(&request.messages),
            "grammar": schema_to_gbnf(request.json_schema.as_ref()),
            "temperature": request.sampling.temperature,
            "n_predict": request.sampling.max_tokens.unwrap_or(512),
            "stream": false,
        })
    }
}

#[async_trait]
impl AiProvider for LlamaCppAdapter {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn capabilities(&self) -> ProviderCapabilities {
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
        let url = format!("{}/completion", self.endpoint);
        let response = self
            .http
            .post_json(
                &url,
                vec![("content-type".to_owned(), "application/json".to_owned())],
                self.body(&request),
            )
            .await?;
        let content = response
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| AiError::Validation("response missing content".to_owned()))?;
        structured_from_content(content, "gbnf_grammar")
    }
}
