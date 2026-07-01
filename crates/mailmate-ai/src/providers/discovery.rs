//! Provider **model discovery**: list the models an endpoint currently serves, so the options
//! UI can offer a pick-list instead of a free-text field. Discovery is a GET against the
//! provider's catalog endpoint; like the rest of each provider's wire knowledge (endpoint paths,
//! response shapes) it lives ONLY here, never in the host. The host calls [`list_models`] with a
//! kind + endpoint (+ an optional key for an authenticated cloud catalog) and gets back plain
//! model names — it learns no provider-specific detail.
//!
//! It degrades, never fabricates: an unreachable endpoint or an unparseable body surfaces as an
//! error (the UI keeps its free-text field); a 200 with an unexpected shape yields an empty list.

use std::collections::HashSet;

use serde_json::Value;

use mailmate_common::error::AiError;
use mailmate_common::secret::Secret;

use crate::http::HttpClient;

/// List the model names the provider of `kind` serves at `endpoint`.
///
/// `endpoint` is the same base the matching adapter uses (e.g. `http://localhost:11434` for
/// Ollama, `http://localhost:1234/v1` for an OpenAI-shaped server). `api_key`, when present,
/// authenticates a cloud catalog (`openai_compatible`).
///
/// # Errors
/// [`AiError::RequestFailed`] if the catalog endpoint is unreachable / non-JSON, or
/// (`code = "unsupported"`) for a kind with no discovery endpoint (`mock`).
pub async fn list_models(
    kind: &str,
    endpoint: &str,
    api_key: Option<&Secret>,
    http: &dyn HttpClient,
) -> Result<Vec<String>, AiError> {
    let base = endpoint.trim_end_matches('/');
    match kind {
        // Ollama's native tag list: `{ "models": [ { "name": "llama3:latest" }, … ] }`.
        "ollama" => {
            let body = http
                .get_json(&format!("{base}/api/tags"), Vec::new())
                .await?;
            Ok(collect(body.pointer("/models"), "name"))
        }
        // OpenAI-shaped `/models` (the base already carries `/v1`): `{ "data": [ { "id": … } ] }`.
        "lm_studio" | "openai_compatible" => {
            let body = http
                .get_json(&format!("{base}/models"), bearer(api_key))
                .await?;
            Ok(collect(body.pointer("/data"), "id"))
        }
        // llama.cpp's server exposes the OpenAI catalog under `/v1` (its completions path is not).
        "llama_cpp" => {
            let body = http
                .get_json(&format!("{base}/v1/models"), Vec::new())
                .await?;
            Ok(collect(body.pointer("/data"), "id"))
        }
        other => Err(AiError::RequestFailed {
            code: "unsupported".to_owned(),
            message: format!("model discovery is not supported for provider kind {other:?}"),
        }),
    }
}

/// The `Authorization: Bearer …` header for an authenticated catalog, or no header.
fn bearer(api_key: Option<&Secret>) -> Vec<(String, String)> {
    match api_key {
        Some(key) => vec![(
            "authorization".to_owned(),
            format!("Bearer {}", key.expose()),
        )],
        None => Vec::new(),
    }
}

/// Pull `field` from each object in the JSON `array`, dropping blanks and duplicates while
/// preserving the provider's order (Ollama lists most-recent first — worth keeping).
fn collect(array: Option<&Value>, field: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut names = Vec::new();
    for item in array.and_then(Value::as_array).into_iter().flatten() {
        if let Some(name) = item.get(field).and_then(Value::as_str) {
            let name = name.trim();
            if !name.is_empty() && seen.insert(name.to_owned()) {
                names.push(name.to_owned());
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use serde_json::json;

    use crate::http::CannedHttpClient;

    #[test]
    fn ollama_lists_tag_names_and_hits_api_tags() {
        let http = CannedHttpClient::new(json!({
            "models": [{ "name": "llama3:latest" }, { "name": "qwen2.5:7b" }]
        }));
        let models =
            block_on(list_models("ollama", "http://localhost:11434", None, &http)).unwrap();
        assert_eq!(models, vec!["llama3:latest", "qwen2.5:7b"]);
        let req = http.last_request().unwrap();
        assert_eq!(req.url, "http://localhost:11434/api/tags");
        assert!(
            req.headers.is_empty(),
            "local discovery sends no auth header"
        );
    }

    #[test]
    fn a_trailing_slash_on_the_endpoint_is_trimmed() {
        let http = CannedHttpClient::new(json!({ "models": [] }));
        let _ = block_on(list_models(
            "ollama",
            "http://localhost:11434/",
            None,
            &http,
        ));
        assert_eq!(
            http.last_request().unwrap().url,
            "http://localhost:11434/api/tags",
            "no doubled slash"
        );
    }

    #[test]
    fn openai_compatible_lists_data_ids_and_sends_the_bearer_key() {
        let http = CannedHttpClient::new(json!({
            "data": [{ "id": "gpt-4o-mini" }, { "id": "gpt-4o" }]
        }));
        let key = Secret::new("sk-secret");
        let models = block_on(list_models(
            "openai_compatible",
            "https://api.openai.com/v1",
            Some(&key),
            &http,
        ))
        .unwrap();
        assert_eq!(models, vec!["gpt-4o-mini", "gpt-4o"]);
        let req = http.last_request().unwrap();
        assert_eq!(req.url, "https://api.openai.com/v1/models");
        assert!(
            req.headers
                .iter()
                .any(|(k, v)| k == "authorization" && v == "Bearer sk-secret"),
            "the stored key authenticates the cloud catalog: {:?}",
            req.headers
        );
    }

    #[test]
    fn lm_studio_uses_the_openai_models_path() {
        let http = CannedHttpClient::new(json!({ "data": [{ "id": "local-model" }] }));
        let models = block_on(list_models(
            "lm_studio",
            "http://localhost:1234/v1",
            None,
            &http,
        ))
        .unwrap();
        assert_eq!(models, vec!["local-model"]);
        assert_eq!(
            http.last_request().unwrap().url,
            "http://localhost:1234/v1/models"
        );
    }

    #[test]
    fn llama_cpp_reaches_the_v1_catalog() {
        let http = CannedHttpClient::new(json!({ "data": [{ "id": "served" }] }));
        let models = block_on(list_models(
            "llama_cpp",
            "http://localhost:8080",
            None,
            &http,
        ))
        .unwrap();
        assert_eq!(models, vec!["served"]);
        assert_eq!(
            http.last_request().unwrap().url,
            "http://localhost:8080/v1/models"
        );
    }

    #[test]
    fn blanks_and_duplicates_are_dropped_order_preserved() {
        let http = CannedHttpClient::new(json!({
            "models": [
                { "name": "b" }, { "name": "" }, { "name": "a" }, { "name": "b" }, { "no_name": 1 }
            ]
        }));
        let models = block_on(list_models("ollama", "http://x", None, &http)).unwrap();
        assert_eq!(models, vec!["b", "a"], "deduped, blanks gone, order kept");
    }

    #[test]
    fn an_unexpected_2xx_shape_yields_an_empty_list_not_an_error() {
        let http = CannedHttpClient::new(json!({ "unexpected": true }));
        let models = block_on(list_models("ollama", "http://x", None, &http)).unwrap();
        assert!(models.is_empty());
    }

    #[test]
    fn an_undiscoverable_kind_is_an_unsupported_error() {
        let http = CannedHttpClient::new(json!({}));
        let err = block_on(list_models("mock", "http://x", None, &http)).unwrap_err();
        match err {
            AiError::RequestFailed { code, .. } => assert_eq!(code, "unsupported"),
            other => panic!("expected RequestFailed/unsupported, got {other:?}"),
        }
    }
}
