//! The Ollama adapter: structured output via `format: "json"` against `/api/chat`.
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
use crate::providers::structured_from_content;

/// An adapter for a local Ollama server.
pub struct OllamaAdapter {
    id: ProviderId,
    endpoint: String,
    model: String,
    http: Arc<dyn HttpClient>,
}

impl OllamaAdapter {
    /// Build the adapter. `endpoint` is the base (e.g. `http://localhost:11434`).
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
            "format": "json",
            "stream": false,
            "options": { "temperature": request.sampling.temperature },
        })
    }
}

#[async_trait]
impl AiProvider for OllamaAdapter {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            grammar: false,
            json_schema: false,
            function_calling: false,
            max_context_tokens: None,
        }
    }

    async fn complete_structured(
        &self,
        request: StructuredRequest,
    ) -> Result<StructuredResponse, AiError> {
        let url = format!("{}/api/chat", self.endpoint);
        let response = self
            .http
            .post_json(
                &url,
                vec![("content-type".to_owned(), "application/json".to_owned())],
                self.body(&request),
            )
            .await?;
        let content = response
            .pointer("/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| AiError::Validation("response missing message.content".to_owned()))?;
        structured_from_content(content, "format_json")
    }
}
