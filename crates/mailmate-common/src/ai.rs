//! The AI-provider vocabulary: the single structured-completion primitive's request and
//! response, provider identity and capabilities, and the prompt/sampling types.
//!
//! These are backend-free value types shared by the `AiProvider` port and every adapter
//! (mock, Ollama, OpenAI-compatible, LM Studio, llama.cpp, and the feature-gated Burn
//! provider). The provider exposes exactly one primitive — `complete_structured` — and all
//! task semantics (classify/draft/summarize/extract/propose) live above it as typed
//! functions over [`StructuredResponse`].

use serde::{Deserialize, Serialize};

/// A provider's configured identity (e.g. `ollama`, `mock`, `openai_compatible`). A plain
/// name, not a prefixed-uuid id — it keys the [`ProviderRegistry`](crate) and `[ai]` config.
///
/// [`ProviderRegistry`]: crate
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ProviderId(String);

impl ProviderId {
    /// Borrow the underlying name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ProviderId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for ProviderId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What structured-output enforcement a provider supports. Advisory — the task layer
/// validates every response regardless of what a provider claims here.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderCapabilities {
    /// Compiles a GBNF grammar from the schema (llama.cpp, LM Studio): guaranteed-valid.
    pub grammar: bool,
    /// Native `response_format: json_schema` (OpenAI-compatible).
    pub json_schema: bool,
    /// Native function/tool calling.
    pub function_calling: bool,
    /// Maximum context window, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<usize>,
}

/// The role of a prompt message.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    /// System/instruction message.
    System,
    /// User message.
    User,
    /// Prior assistant message.
    Assistant,
}

/// One prompt message.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptMessage {
    /// The role.
    pub role: MessageRole,
    /// The message content.
    pub content: String,
}

impl PromptMessage {
    /// A system message.
    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: content.into(),
        }
    }

    /// A user message.
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
        }
    }
}

/// Sampling parameters. Defaults to deterministic decoding (temperature 0).
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct SamplingParams {
    /// Sampling temperature; 0 = greedy/deterministic.
    pub temperature: f32,
    /// Maximum tokens to generate, if bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Nucleus-sampling top-p, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
}

impl Default for SamplingParams {
    fn default() -> Self {
        Self {
            temperature: 0.0,
            max_tokens: None,
            top_p: None,
        }
    }
}

/// A request for one structured completion.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StructuredRequest {
    /// The prompt messages (system + user).
    pub messages: Vec<PromptMessage>,
    /// The JSON schema the output must satisfy, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<serde_json::Value>,
    /// A precompiled grammar, if the caller supplies one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grammar: Option<String>,
    /// Sampling parameters.
    #[serde(default)]
    pub sampling: SamplingParams,
}

impl StructuredRequest {
    /// A schema-enforced request from a message list and a JSON schema.
    #[must_use]
    pub fn new(messages: Vec<PromptMessage>, json_schema: serde_json::Value) -> Self {
        Self {
            messages,
            json_schema: Some(json_schema),
            grammar: None,
            sampling: SamplingParams::default(),
        }
    }
}

/// The result of one structured completion: the raw text, the parsed JSON, and which
/// enforcement method validated it (for the audit trail).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StructuredResponse {
    /// The model's raw text output.
    pub raw_text: String,
    /// The parsed JSON value (the task layer deserializes this into a typed struct).
    pub parsed_json: serde_json::Value,
    /// Which enforcement method produced/validated the JSON (e.g. `gbnf_grammar`,
    /// `json_schema`, `format_json`, `prompt_repair`, `mock`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_validated_by: Option<String>,
}

impl StructuredResponse {
    /// Build a response from raw text by parsing it as JSON, tagging the enforcement
    /// method. Returns a codec error if the text is not valid JSON — the parse failure a
    /// weak-backend adapter surfaces.
    ///
    /// # Errors
    /// [`crate::error::AiError::Codec`] if `raw_text` is not valid JSON.
    pub fn from_raw_json(
        raw_text: impl Into<String>,
        method: impl Into<String>,
    ) -> Result<Self, crate::error::AiError> {
        let raw_text = raw_text.into();
        let parsed_json = serde_json::from_str(&raw_text)
            .map_err(|e| crate::error::AiError::Codec(e.to_string()))?;
        Ok(Self {
            raw_text,
            parsed_json,
            schema_validated_by: Some(method.into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_id_round_trips_as_a_bare_string() {
        let id = ProviderId::from("ollama");
        assert_eq!(id.as_str(), "ollama");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"ollama\"");
        assert_eq!(id.to_string(), "ollama");
    }

    #[test]
    fn sampling_defaults_to_deterministic() {
        assert_eq!(SamplingParams::default().temperature, 0.0);
    }

    #[test]
    fn structured_response_from_raw_json_parses_or_errors() {
        let ok = StructuredResponse::from_raw_json(r#"{"a":1}"#, "mock").unwrap();
        assert_eq!(ok.parsed_json, json!({ "a": 1 }));
        assert_eq!(ok.schema_validated_by.as_deref(), Some("mock"));

        let err = StructuredResponse::from_raw_json("not json {", "mock");
        assert!(err.is_err(), "invalid JSON must error");
    }

    #[test]
    fn request_builder_sets_schema() {
        let req = StructuredRequest::new(
            vec![
                PromptMessage::system("you are MailMate"),
                PromptMessage::user("classify"),
            ],
            json!({ "type": "object" }),
        );
        assert_eq!(req.messages.len(), 2);
        assert!(req.json_schema.is_some());
    }
}
