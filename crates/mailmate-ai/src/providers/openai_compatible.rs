//! The OpenAI-compatible adapter: native `response_format: json_schema`, Bearer auth from
//! a [`Secret`]. Endpoint, model, and key are adapter-private and never leak.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use mailmate_common::ai::{
    ProviderCapabilities, ProviderId, StructuredRequest, StructuredResponse,
};
use mailmate_common::error::AiError;
use mailmate_common::secret::Secret;
use mailmate_ports::ai_provider::AiProvider;

use crate::http::HttpClient;
use crate::providers::structured_from_content;

/// An adapter for any OpenAI-compatible chat-completions endpoint.
pub struct OpenAiCompatibleAdapter {
    id: ProviderId,
    endpoint: String,
    model: String,
    api_key: Option<Secret>,
    http: Arc<dyn HttpClient>,
}

impl OpenAiCompatibleAdapter {
    /// Build the adapter. `endpoint` is the base (e.g. `https://api.example.com/v1`).
    #[must_use]
    pub fn new(
        id: impl Into<ProviderId>,
        endpoint: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<Secret>,
        http: Arc<dyn HttpClient>,
    ) -> Self {
        Self {
            id: id.into(),
            endpoint: endpoint.into(),
            model: model.into(),
            api_key,
            http,
        }
    }

    fn body(&self, request: &StructuredRequest) -> Value {
        let mut body = json!({
            "model": self.model,
            "messages": request.messages,
            "temperature": request.sampling.temperature,
        });
        if let Some(schema) = &request.json_schema {
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": { "name": "mailmate_task", "schema": schema, "strict": true }
            });
        }
        body
    }

    fn headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![("content-type".to_owned(), "application/json".to_owned())];
        if let Some(key) = &self.api_key {
            headers.push((
                "authorization".to_owned(),
                format!("Bearer {}", key.expose()),
            ));
        }
        headers
    }
}

#[async_trait]
impl AiProvider for OpenAiCompatibleAdapter {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            grammar: false,
            json_schema: true,
            function_calling: true,
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
            .post_json(&url, self.headers(), self.body(&request))
            .await?;
        let content = response
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AiError::Validation("response missing choices[0].message.content".to_owned())
            })?;
        structured_from_content(content, "json_schema")
    }
}
