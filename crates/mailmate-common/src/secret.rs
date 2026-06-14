//! Secret keys and values for the `SecretStore` port.
//!
//! [`Secret`]'s `Debug` is redacted so a secret can never leak into a log line or a
//! panic message by accident.

use serde::{Deserialize, Serialize};

/// The lookup key for a stored secret (e.g. a provider's API-key slot).
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SecretKey(pub String);

impl SecretKey {
    /// Borrow the key string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SecretKey {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// A secret value. `Debug` is redacted; use [`expose`](Secret::expose) deliberately.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    /// Wrap a secret value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Reveal the secret. Call sites are the audit surface for secret handling.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_the_value() {
        let secret = Secret::new("super-sensitive-token");
        assert_eq!(format!("{secret:?}"), "Secret(***)");
        assert!(!format!("{secret:?}").contains("super-sensitive"));
    }

    #[test]
    fn expose_returns_the_value_deliberately() {
        let secret = Secret::new("abc");
        assert_eq!(secret.expose(), "abc");
    }

    #[test]
    fn secret_serializes_transparently() {
        // It must round-trip for storage, even though Debug hides it.
        let secret = Secret::new("abc");
        assert_eq!(serde_json::to_string(&secret).unwrap(), "\"abc\"");
        let back: Secret = serde_json::from_str("\"abc\"").unwrap();
        assert_eq!(back, secret);
    }

    #[test]
    fn secret_key_borrows_and_converts() {
        let key = SecretKey::from("ollama_api_key");
        assert_eq!(key.as_str(), "ollama_api_key");
    }
}
