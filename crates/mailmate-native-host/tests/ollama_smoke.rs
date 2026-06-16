//! A LIVE smoke test against a real local Ollama on `http://localhost:11434` — it proves the
//! std HTTP transport + the Ollama adapter round-trip end to end through the real
//! [`build_provider`](mailmate_native_host::provider::build_provider) wiring.
//!
//! `#[ignore]` so it NEVER runs in the gate (it needs a running Ollama with a pulled model and
//! does real network I/O). Run it explicitly:
//!
//! ```text
//! cargo test -p mailmate-native-host --test ollama_smoke -- --ignored --nocapture
//! ```
//!
//! Override the model with `MAILMATE_SMOKE_MODEL` (default: a small local model).

use std::sync::Arc;

use futures::executor::block_on;

use mailmate_common::ai::{PromptMessage, SamplingParams, StructuredRequest};
use mailmate_test_support::fakes::FakeSecretStore;

use mailmate_native_host::config::{AiSettings, ProviderSettings};
use mailmate_native_host::http_client::StdHttpClient;
use mailmate_native_host::provider::build_provider;

#[test]
#[ignore = "needs a local Ollama on http://localhost:11434 with the model pulled"]
fn build_provider_then_complete_against_real_ollama() {
    let model = std::env::var("MAILMATE_SMOKE_MODEL").unwrap_or_else(|_| "lfm2:latest".to_owned());

    // The real composition path: settings → build_provider → OllamaAdapter over StdHttpClient.
    let ai = AiSettings {
        default_provider: Some("local".to_owned()),
        providers: vec![ProviderSettings {
            id: "local".to_owned(),
            kind: "ollama".to_owned(),
            endpoint: Some("http://localhost:11434".to_owned()),
            model: Some(model.clone()),
        }],
    };
    let provider = build_provider(&ai, &FakeSecretStore::new(), Arc::new(StdHttpClient::new()));
    assert_ne!(
        provider.id().to_string(),
        "unavailable",
        "a complete config must build a real adapter, not the unavailable stand-in"
    );

    // Ollama's `format: "json"` forces valid JSON, so the adapter returns a parsed object.
    let request = StructuredRequest {
        messages: vec![PromptMessage::user(
            "Reply with a JSON object that has a single key \"greeting\" whose value is the \
             string \"hello\". Output only the JSON object.",
        )],
        json_schema: None,
        grammar: None,
        sampling: SamplingParams::default(),
    };

    let response = block_on(provider.complete_structured(request))
        .unwrap_or_else(|e| panic!("real Ollama round-trip failed for model `{model}`: {e}"));

    eprintln!(
        "[smoke] model `{model}` returned parsed JSON: {}",
        response.parsed_json
    );
    assert!(
        !response.raw_text.is_empty(),
        "expected a non-empty response from the model"
    );
}
