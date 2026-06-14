//! Adapter tests for the four HTTP providers, driven through the `CannedHttpClient` seam —
//! no network. Each test asserts request *shaping* (the provider-specific body) and
//! response *parsing* (extracting the inner JSON), plus advertised capabilities.

use std::sync::Arc;

use futures::executor::block_on;
use serde_json::{json, Value};

use mailmate_ai::http::CannedHttpClient;
use mailmate_ai::providers::{
    LlamaCppAdapter, LmStudioAdapter, OllamaAdapter, OpenAiCompatibleAdapter,
};
use mailmate_ai::ProviderRegistry;
use mailmate_common::ai::{PromptMessage, ProviderId, SamplingParams, StructuredRequest};
use mailmate_common::secret::Secret;
use mailmate_ports::ai_provider::AiProvider;

fn request() -> StructuredRequest {
    StructuredRequest::new(
        vec![
            PromptMessage::system("be MailMate"),
            PromptMessage::user("classify"),
        ],
        json!({ "type": "object" }),
    )
}

/// The structured payload a model "returns" (as the inner content string).
fn inner() -> Value {
    json!({ "labels": ["receipt"], "spam_score": 0.0, "phishing_score": 0.0, "priority": "low" })
}

fn openai_envelope() -> Value {
    json!({ "choices": [{ "message": { "content": inner().to_string() } }] })
}

#[test]
fn openai_compatible_shapes_response_format_and_parses_content() {
    let http = Arc::new(CannedHttpClient::new(openai_envelope()));
    let adapter = OpenAiCompatibleAdapter::new(
        "openai",
        "https://api.example.com/v1",
        "gpt-x",
        Some(Secret::new("sk-secret")),
        http.clone(),
    );
    assert!(adapter.capabilities().json_schema);

    let resp = block_on(adapter.complete_structured(request())).unwrap();
    assert_eq!(resp.parsed_json, inner());
    assert_eq!(resp.schema_validated_by.as_deref(), Some("json_schema"));

    let sent = http.last_request().unwrap();
    assert!(sent.url.ends_with("/chat/completions"));
    assert_eq!(sent.body["model"], "gpt-x");
    assert_eq!(sent.body["response_format"]["type"], "json_schema");
    // The API key rides an Authorization header, never the body.
    assert!(sent
        .headers
        .iter()
        .any(|(k, v)| k == "authorization" && v.starts_with("Bearer ")));
    assert!(
        !sent.body.to_string().contains("sk-secret"),
        "key must not leak into the body"
    );
}

#[test]
fn ollama_shapes_format_json_and_parses_message_content() {
    let http = Arc::new(CannedHttpClient::new(
        json!({ "message": { "content": inner().to_string() } }),
    ));
    let adapter = OllamaAdapter::new("ollama", "http://localhost:11434", "llama3", http.clone());
    assert!(!adapter.capabilities().json_schema);

    let resp = block_on(adapter.complete_structured(request())).unwrap();
    assert_eq!(resp.parsed_json, inner());
    assert_eq!(resp.schema_validated_by.as_deref(), Some("format_json"));

    let sent = http.last_request().unwrap();
    assert!(sent.url.ends_with("/api/chat"));
    assert_eq!(sent.body["format"], "json");
}

#[test]
fn lm_studio_sends_a_grammar_and_parses_content() {
    let http = Arc::new(CannedHttpClient::new(openai_envelope()));
    let adapter = LmStudioAdapter::new(
        "lmstudio",
        "http://localhost:1234/v1",
        "local-model",
        http.clone(),
    );
    assert!(adapter.capabilities().grammar);

    let resp = block_on(adapter.complete_structured(request())).unwrap();
    assert_eq!(resp.parsed_json, inner());
    assert_eq!(resp.schema_validated_by.as_deref(), Some("gbnf_grammar"));

    let sent = http.last_request().unwrap();
    assert!(sent.body["grammar"].as_str().unwrap().contains("root ::="));
}

#[test]
fn llama_cpp_sends_prompt_plus_grammar_and_parses_content() {
    let http = Arc::new(CannedHttpClient::new(
        json!({ "content": inner().to_string() }),
    ));
    let adapter = LlamaCppAdapter::new("llamacpp", "http://localhost:8080", http.clone());
    assert!(adapter.capabilities().grammar);

    let resp = block_on(adapter.complete_structured(request())).unwrap();
    assert_eq!(resp.parsed_json, inner());

    let sent = http.last_request().unwrap();
    assert!(sent.url.ends_with("/completion"));
    assert!(sent.body["prompt"].as_str().unwrap().contains("SYSTEM:"));
    assert!(sent.body["grammar"].as_str().unwrap().contains("root ::="));
}

#[test]
fn a_malformed_provider_response_is_rejected_by_the_adapter() {
    // The model emitted content that is not valid JSON → the adapter fails closed.
    let http = Arc::new(CannedHttpClient::new(
        json!({ "message": { "content": "this is not json {" } }),
    ));
    let adapter = OllamaAdapter::new("ollama", "http://localhost:11434", "llama3", http);
    assert!(block_on(adapter.complete_structured(request())).is_err());
}

#[test]
fn all_four_adapters_register_in_one_registry() {
    let http: Arc<CannedHttpClient> = Arc::new(CannedHttpClient::new(openai_envelope()));
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(OpenAiCompatibleAdapter::new(
        "openai",
        "https://x/v1",
        "m",
        None,
        http.clone(),
    )));
    registry.register(Box::new(OllamaAdapter::new(
        "ollama",
        "http://x",
        "m",
        http.clone(),
    )));
    registry.register(Box::new(LmStudioAdapter::new(
        "lmstudio",
        "http://x/v1",
        "m",
        http.clone(),
    )));
    registry.register(Box::new(LlamaCppAdapter::new("llamacpp", "http://x", http)));

    assert_eq!(registry.len(), 4);
    for id in ["openai", "ollama", "lmstudio", "llamacpp"] {
        assert!(
            registry.get(&ProviderId::from(id)).is_some(),
            "{id} should be registered"
        );
    }
    let sampling = StructuredRequest::new(vec![], json!({})).sampling;
    assert_eq!(sampling, SamplingParams::default());
}
