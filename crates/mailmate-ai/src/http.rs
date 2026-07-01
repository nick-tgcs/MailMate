//! The HTTP seam beneath the network providers.
//!
//! The four remote adapters (Ollama, OpenAI-compatible, LM Studio, llama.cpp) do their
//! provider-specific request *shaping* and response *parsing* in pure code, and reach the
//! network only through this tiny [`HttpClient`] port. That keeps the whole HTTP/TLS stack out
//! of the default build entirely: tests drive the adapters with [`CannedHttpClient`], and the
//! concrete `ureq`-backed client is wired in at the edge (the host binary) where real network
//! I/O belongs. This mirrors how the repositories sit above the `StorageBackend` seam — the
//! provider logic never touches a socket directly.

use async_trait::async_trait;

use mailmate_common::error::AiError;

/// A minimal HTTP JSON transport: a POST for structured completions, a GET for catalog reads.
#[async_trait]
pub trait HttpClient: Send + Sync {
    /// POST `body` as JSON to `url` with `headers`, returning the parsed JSON response.
    ///
    /// # Errors
    /// [`AiError::RequestFailed`] on transport failure or a non-JSON response.
    async fn post_json(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, AiError>;

    /// GET `url` with `headers`, returning the parsed JSON response. Provider *model discovery*
    /// (listing the models an endpoint serves) reads a catalog endpoint, which is a GET — the
    /// completions path is the POST above. Provider-specific URLs/shapes live in
    /// [`providers::discovery`](crate::providers::discovery), not here.
    ///
    /// # Errors
    /// [`AiError::RequestFailed`] on transport failure or a non-JSON response.
    async fn get_json(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
    ) -> Result<serde_json::Value, AiError>;
}

/// A deterministic [`HttpClient`] that returns a fixed canned response and records the last
/// request — for tests and offline/demo modes. No network.
#[derive(Debug)]
pub struct CannedHttpClient {
    response: serde_json::Value,
    last_request: std::sync::Mutex<Option<CapturedRequest>>,
}

/// What [`CannedHttpClient`] last received, so a test can assert request shaping.
#[derive(Clone, Debug)]
pub struct CapturedRequest {
    /// The URL posted to.
    pub url: String,
    /// The headers sent.
    pub headers: Vec<(String, String)>,
    /// The JSON body sent.
    pub body: serde_json::Value,
}

impl CannedHttpClient {
    /// A client that always returns `response`.
    #[must_use]
    pub fn new(response: serde_json::Value) -> Self {
        Self {
            response,
            last_request: std::sync::Mutex::new(None),
        }
    }

    /// The most recent request this client received (for assertions).
    #[must_use]
    pub fn last_request(&self) -> Option<CapturedRequest> {
        self.last_request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl HttpClient for CannedHttpClient {
    async fn post_json(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, AiError> {
        self.record(url, headers, body);
        Ok(self.response.clone())
    }

    async fn get_json(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
    ) -> Result<serde_json::Value, AiError> {
        // A GET carries no body; record `Null` so a test can still assert the url + headers.
        self.record(url, headers, serde_json::Value::Null);
        Ok(self.response.clone())
    }
}

impl CannedHttpClient {
    /// Record the last request (shared by [`post_json`](HttpClient::post_json) and
    /// [`get_json`](HttpClient::get_json)).
    fn record(&self, url: &str, headers: Vec<(String, String)>, body: serde_json::Value) {
        *self
            .last_request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CapturedRequest {
            url: url.to_owned(),
            headers,
            body,
        });
    }
}
