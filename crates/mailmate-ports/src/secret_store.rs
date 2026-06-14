//! The secret-store port: read/write credentials (e.g. provider API keys).

use async_trait::async_trait;

use mailmate_common::error::SecretError;
use mailmate_common::secret::{Secret, SecretKey};

/// A store for secret values.
///
/// Adapters: a 0600 config-dir file (default), the OS keychain, an env-var override
/// (dev), and an in-memory mock. The core only ever sees this trait.
#[async_trait]
pub trait SecretStore: Send + Sync {
    /// Fetch a secret, or `None` if no value is stored for `key`.
    async fn get(&self, key: SecretKey) -> Result<Option<Secret>, SecretError>;

    /// Store (or overwrite) a secret value for `key`.
    async fn put(&self, key: SecretKey, value: Secret) -> Result<(), SecretError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn SecretStore) {}
        let _ = takes as fn(&dyn SecretStore);
    }
}
