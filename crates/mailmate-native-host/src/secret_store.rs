//! The production [`SecretStore`]: a `0600` JSON file in the data directory.
//!
//! Provider API keys are the only secrets MailMate holds, and only when a remote provider is
//! configured (the zero-provider default never touches this). They live in a single
//! owner-only (`0600` on Unix) JSON file beside the database — not in the TOML config, so a
//! shared/committed config never carries a key. The store is lazy: a missing file reads as
//! "no secrets", so pointing the composition root at a not-yet-created path is harmless.

use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;

use mailmate_common::error::SecretError;
use mailmate_common::secret::{Secret, SecretKey};
use mailmate_ports::secret_store::SecretStore;

/// A file-backed secret store.
#[derive(Clone, Debug)]
pub struct FileSecretStore {
    path: PathBuf,
}

impl FileSecretStore {
    /// A store backed by the JSON file at `path` (created on first `put`).
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Read the whole key→value map, or an empty map when the file does not exist.
    fn read_all(&self) -> Result<BTreeMap<String, String>, SecretError> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| SecretError::Malformed(format!("secret file is not valid JSON: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(SecretError::Backend(format!(
                "reading {}: {e}",
                self.path.display()
            ))),
        }
    }

    /// Write the whole map back, creating the parent directory and the file owner-only
    /// (`0600`) on Unix.
    fn write_all(&self, map: &BTreeMap<String, String>) -> Result<(), SecretError> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| SecretError::Backend(format!("creating secret directory: {e}")))?;
            }
        }
        let text = serde_json::to_string_pretty(map)
            .map_err(|e| SecretError::Backend(format!("serializing secrets: {e}")))?;
        self.write_owner_only(text.as_bytes())
    }

    /// The sibling temp path the owner-only write stages through (same directory, so the
    /// final `rename` is atomic on one filesystem).
    fn tmp_path(&self) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(".tmp");
        PathBuf::from(name)
    }

    /// Write `bytes` so the secret file is **never** momentarily world-readable: stage into a
    /// temp created `0600` from the start, then atomically rename over the target. This closes
    /// the create-at-umask-then-chmod window (a local process could otherwise read the key in
    /// between) and avoids truncating the live file on a mid-write crash.
    #[cfg(unix)]
    fn write_owner_only(&self, bytes: &[u8]) -> Result<(), SecretError> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let tmp = self.tmp_path();
        {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .map_err(|e| SecretError::Backend(format!("creating secret temp file: {e}")))?;
            file.write_all(bytes)
                .map_err(|e| SecretError::Backend(format!("writing secret temp file: {e}")))?;
        }
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| SecretError::Backend(format!("installing the secret file: {e}")))
    }

    #[cfg(not(unix))]
    fn write_owner_only(&self, bytes: &[u8]) -> Result<(), SecretError> {
        std::fs::write(&self.path, bytes)
            .map_err(|e| SecretError::Backend(format!("writing {}: {e}", self.path.display())))
    }
}

#[async_trait]
impl SecretStore for FileSecretStore {
    async fn get(&self, key: SecretKey) -> Result<Option<Secret>, SecretError> {
        Ok(self.read_all()?.get(key.as_str()).map(Secret::new))
    }

    async fn put(&self, key: SecretKey, value: Secret) -> Result<(), SecretError> {
        let mut map = self.read_all()?;
        map.insert(key.0, value.expose().to_owned());
        self.write_all(&map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    #[test]
    fn put_then_get_round_trips_and_a_missing_key_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().join("secrets").join("store.json"));

        // Absent file reads as "no secrets".
        assert!(block_on(store.get(SecretKey::from("ollama_api_key")))
            .unwrap()
            .is_none());

        block_on(store.put(SecretKey::from("ollama_api_key"), Secret::new("tok-123"))).unwrap();
        let got = block_on(store.get(SecretKey::from("ollama_api_key")))
            .unwrap()
            .unwrap();
        assert_eq!(got.expose(), "tok-123");
        // A different key is still absent.
        assert!(block_on(store.get(SecretKey::from("other")))
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_second_put_preserves_other_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().join("store.json"));
        block_on(store.put(SecretKey::from("a"), Secret::new("1"))).unwrap();
        block_on(store.put(SecretKey::from("b"), Secret::new("2"))).unwrap();
        assert_eq!(
            block_on(store.get(SecretKey::from("a")))
                .unwrap()
                .unwrap()
                .expose(),
            "1"
        );
        assert_eq!(
            block_on(store.get(SecretKey::from("b")))
                .unwrap()
                .unwrap()
                .expose(),
            "2"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_secret_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.json");
        let store = FileSecretStore::new(&path);
        block_on(store.put(SecretKey::from("k"), Secret::new("v"))).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "secret file must be 0600");
    }

    #[cfg(unix)]
    #[test]
    fn writing_over_a_world_readable_file_re_locks_it_to_0600() {
        // The atomic temp+rename must enforce 0600 even when overwriting a pre-existing,
        // loosely-permissioned file — and never leave a window where the new secret is
        // world-readable.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let store = FileSecretStore::new(&path);
        block_on(store.put(SecretKey::from("k"), Secret::new("v"))).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "overwrite must re-lock to 0600");
        // No stray temp file is left behind.
        assert!(!path.with_extension("json.tmp").exists());
        assert_eq!(
            block_on(store.get(SecretKey::from("k")))
                .unwrap()
                .unwrap()
                .expose(),
            "v"
        );
    }

    #[test]
    fn a_malformed_secret_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.json");
        std::fs::write(&path, "not json").unwrap();
        let store = FileSecretStore::new(&path);
        assert!(matches!(
            block_on(store.get(SecretKey::from("k"))),
            Err(SecretError::Malformed(_))
        ));
    }
}
