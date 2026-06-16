//! A blocking [`HttpClient`] over [`ureq`] — the host-side concrete LLM transport.
//!
//! mailmate-ai's four LLM adapters (Ollama, LM Studio, llama.cpp, OpenAI-compatible) reach the
//! network only through the [`HttpClient`] seam; the concrete transport belongs at the edge.
//! `ureq` is a blocking, `rustls`-backed HTTP/1.1 client with **no async runtime** — a natural
//! fit for the host's serial `block_on` loop (a synchronous request cannot starve a concurrent
//! peer; there isn't one). It speaks both `http://` (local providers — Ollama / LM Studio /
//! llama.cpp on localhost) and `https://` (cloud OpenAI-compatible / Ollama cloud), with the
//! Mozilla root store bundled (`webpki-roots`), so TLS needs zero configuration.
//!
//! This replaced a hand-rolled `std::net` HTTP/1.1 client: HTTP framing and TLS are solved
//! problems, so we use the crate rather than reimplement chunked decoding and a TLS handshake.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use ureq::Agent;

use mailmate_ai::http::HttpClient;
use mailmate_common::error::AiError;

/// The default per-request (global) timeout. Generous: a cold local model can be slow to first
/// token, and `ureq`'s *global* timeout spans connect + TLS + send + the entire response read.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// How much of an error-response body to echo back in the [`AiError`] message.
const SNIPPET_LEN: usize = 200;

/// A blocking [`HttpClient`] backed by `ureq` (rustls TLS, no async runtime). Holds a single
/// [`Agent`] so connections pool across requests; cloning is cheap (the agent is `Arc`-backed).
#[derive(Clone)]
pub struct StdHttpClient {
    agent: Agent,
}

impl StdHttpClient {
    /// A client with the default 120s timeout.
    #[must_use]
    pub fn new() -> Self {
        Self::with_timeout(DEFAULT_TIMEOUT)
    }

    /// A client with an explicit global per-request timeout (used by tests).
    #[must_use]
    pub fn with_timeout(timeout: Duration) -> Self {
        let config = Agent::config_builder()
            .timeout_global(Some(timeout))
            // We map a non-2xx to `AiError::RequestFailed` ourselves so the error carries a body
            // snippet; ureq's default would collapse it to a bare `StatusCode` with no body.
            .http_status_as_error(false)
            .build();
        Self {
            agent: Agent::new_with_config(config),
        }
    }
}

impl Default for StdHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for StdHttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `ureq::Agent` is not `Debug`; the config is not interesting to print anyway.
        f.debug_struct("StdHttpClient").finish_non_exhaustive()
    }
}

#[async_trait]
impl HttpClient for StdHttpClient {
    async fn post_json(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: Value,
    ) -> Result<Value, AiError> {
        let mut request = self.agent.post(url);
        // Forward caller headers verbatim. `send_json` adds `content-type: application/json` only
        // if absent (see ureq `send`), so the adapters' own content-type is respected, not doubled;
        // OpenAI-compatible's `Authorization: Bearer …` rides through for the https/cloud path.
        for (key, value) in &headers {
            request = request.header(key.as_str(), value.as_str());
        }

        let response = request
            .send_json(&body)
            .map_err(|e| request_failed(ureq_error_code(&e), format!("POST {url} failed: {e}")))?;
        read_json_response(response)
    }

    async fn get_json(&self, url: &str, headers: Vec<(String, String)>) -> Result<Value, AiError> {
        // The GET counterpart of `post_json`, used for provider model discovery (catalog reads).
        // A GET carries no body, so it is `.call()`, not `.send_json(..)`; everything else — header
        // forwarding (incl. `Authorization` for an authenticated cloud catalog), status handling,
        // and JSON decoding — is identical.
        let mut request = self.agent.get(url);
        for (key, value) in &headers {
            request = request.header(key.as_str(), value.as_str());
        }
        let response = request
            .call()
            .map_err(|e| request_failed(ureq_error_code(&e), format!("GET {url} failed: {e}")))?;
        read_json_response(response)
    }
}

/// Interpret a completed `ureq` response: map a non-2xx to an [`AiError::RequestFailed`] carrying
/// the status code + a body snippet, and decode a 2xx body as JSON. Shared by the POST (completions)
/// and GET (model discovery) paths so both fail and decode identically.
fn read_json_response(mut response: ureq::http::Response<ureq::Body>) -> Result<Value, AiError> {
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let text = response.body_mut().read_to_string().unwrap_or_default();
        let snippet: String = text.trim().chars().take(SNIPPET_LEN).collect();
        return Err(request_failed(
            status.to_string(),
            format!("provider returned HTTP {status}: {snippet}"),
        ));
    }
    response
        .body_mut()
        .read_json::<Value>()
        .map_err(|e| request_failed("decode", format!("provider response was not JSON: {e}")))
}

fn request_failed(code: impl Into<String>, message: impl Into<String>) -> AiError {
    AiError::RequestFailed {
        code: code.into(),
        message: message.into(),
    }
}

/// A short, stable code for the common `ureq` failure modes (the message keeps the detail).
/// `ureq::Error` is `#[non_exhaustive]`, so the wildcard is mandatory — and it absorbs the
/// feature-gated variants (native-tls, cookies, …) this build does not enable.
fn ureq_error_code(error: &ureq::Error) -> &'static str {
    match error {
        ureq::Error::Timeout(_) => "timeout",
        ureq::Error::Io(_) => "io",
        ureq::Error::BadUri(_) | ureq::Error::Http(_) => "bad_url",
        ureq::Error::RequireHttpsOnly(_) => "https_required",
        ureq::Error::Json(_) => "decode",
        _ => "request",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::{channel, Receiver};
    use std::thread;

    use futures::executor::block_on;
    use serde_json::json;

    /// A one-shot localhost HTTP server: it accepts one connection, hands the raw request bytes
    /// back over a channel (for assertions), then writes `response` and closes. Hermetic — no
    /// real network. Returns the `/api/chat` URL bound to the ephemeral port.
    fn stub_server(response: Vec<u8>) -> (String, Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = channel();
        thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                // Drain the FULL request before responding: closing a socket that still has unread
                // bytes sends RST (not FIN), which the client would surface as a read error. A short
                // read timeout ends the drain once the request has arrived.
                let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
                let mut buf = [0u8; 8192];
                let mut req = Vec::new();
                loop {
                    match sock.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                        Err(_) => break, // timeout → the request is fully received
                    }
                }
                let _ = tx.send(req);
                let _ = sock.write_all(&response);
            }
        });
        (format!("http://127.0.0.1:{port}/api/chat"), rx)
    }

    fn content_length_response(json_body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json_body}",
            json_body.len()
        )
        .into_bytes()
    }

    #[test]
    fn posts_json_and_parses_a_content_length_response() {
        let (url, rx) = stub_server(content_length_response(r#"{"message":{"content":"hi"}}"#));
        let client = StdHttpClient::with_timeout(Duration::from_secs(5));

        let resp = block_on(client.post_json(
            &url,
            vec![("content-type".to_owned(), "application/json".to_owned())],
            json!({ "model": "llama3" }),
        ))
        .unwrap();
        assert_eq!(
            resp.pointer("/message/content").and_then(Value::as_str),
            Some("hi")
        );

        // The request was a POST to the path, with our JSON body and a single content-type.
        let req = String::from_utf8(rx.recv().unwrap()).unwrap();
        assert!(req.starts_with("POST /api/chat HTTP/1.1"), "got {req:?}");
        let lower = req.to_ascii_lowercase();
        assert!(
            lower.contains("content-type: application/json"),
            "got {req:?}"
        );
        assert_eq!(
            lower.matches("content-type:").count(),
            1,
            "content-type must not be duplicated: {req:?}"
        );
        // Assert on the parsed body, not its byte layout (ureq pretty-prints the JSON).
        let body = &req[req.find("\r\n\r\n").map_or(0, |i| i + 4)..];
        let sent: Value = serde_json::from_str(body.trim()).expect("a JSON request body");
        assert_eq!(sent, json!({ "model": "llama3" }), "got {req:?}");
    }

    #[test]
    fn parses_a_chunked_response() {
        let json_body = r#"{"a":1,"b":"two"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{json_body}\r\n0\r\n\r\n",
            json_body.len()
        )
        .into_bytes();
        let (url, _rx) = stub_server(response);
        let client = StdHttpClient::with_timeout(Duration::from_secs(5));

        let got = block_on(client.post_json(&url, vec![], json!({}))).unwrap();
        assert_eq!(got["a"], 1);
        assert_eq!(got["b"], "two");
    }

    #[test]
    fn get_json_issues_a_get_forwards_headers_and_parses() {
        let (url, rx) = stub_server(content_length_response(r#"{"models":[{"name":"llama3"}]}"#));
        let client = StdHttpClient::with_timeout(Duration::from_secs(5));

        let resp = block_on(client.get_json(
            &url,
            vec![("authorization".to_owned(), "Bearer sk-x".to_owned())],
        ))
        .unwrap();
        assert_eq!(
            resp.pointer("/models/0/name").and_then(Value::as_str),
            Some("llama3")
        );

        // It is a GET (no body) and the auth header rode through (the cloud-catalog path).
        let req = String::from_utf8(rx.recv().unwrap()).unwrap();
        assert!(req.starts_with("GET /api/chat HTTP/1.1"), "got {req:?}");
        assert!(
            req.to_ascii_lowercase()
                .contains("authorization: bearer sk-x"),
            "got {req:?}"
        );
    }

    #[test]
    fn a_non_2xx_status_is_a_request_failed_error_carrying_the_code() {
        let response =
            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5\r\nConnection: close\r\n\r\noops!"
                .to_vec();
        let (url, _rx) = stub_server(response);
        let client = StdHttpClient::with_timeout(Duration::from_secs(5));

        let err = block_on(client.post_json(&url, vec![], json!({}))).unwrap_err();
        match err {
            AiError::RequestFailed { code, .. } => assert_eq!(code, "500"),
            other => panic!("expected RequestFailed, got {other:?}"),
        }
    }

    #[test]
    fn an_https_url_is_attempted_not_pre_rejected() {
        // Bind then drop to obtain a definitely-closed localhost port. A POST to `https://` there
        // must TRY to connect (failing at the socket), NOT bail out up-front the way the old
        // http-only client did — it refused every `https` URL without connecting. So we expect a
        // connection-level failure, never a `bad_url` scheme rejection.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let client = StdHttpClient::with_timeout(Duration::from_secs(5));
        let err = block_on(client.post_json(
            &format!("https://127.0.0.1:{port}/v1/chat/completions"),
            vec![],
            json!({}),
        ))
        .unwrap_err();
        match err {
            AiError::RequestFailed { code, message } => {
                assert_ne!(
                    code, "bad_url",
                    "the https scheme must be accepted, not rejected: {message}"
                );
            }
            other => panic!("expected RequestFailed, got {other:?}"),
        }
    }
}
