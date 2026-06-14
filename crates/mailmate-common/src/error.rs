//! The shared, per-port error taxonomy.
//!
//! Each cross-cutting port returns a concrete error enum (not a boxed `dyn Error` and
//! not an associated type), so the traits stay object-safe and adapters map their
//! native failures onto a stable, matchable surface. Leaf errors fold into
//! [`MailMateError`] via `#[from]` for call sites that want one type.

use crate::ids::MessageId;
use crate::secret::SecretKey;

/// Failures from the `MailClient` port.
#[derive(Debug, thiserror::Error)]
pub enum MailError {
    /// No message with that id.
    #[error("message not found: {0}")]
    NotFound(MessageId),
    /// The client cannot perform the requested action.
    #[error("mail action not supported by the client: {0}")]
    Unsupported(String),
    /// The requested content is not available at the active retention level.
    #[error("content not available under the current retention level")]
    RetentionDenied,
    /// An adapter-specific failure.
    #[error("mail client adapter error: {0}")]
    Adapter(String),
}

/// Failures from the `Transport` port.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// A frame exceeds the 1 MB native-messaging limit.
    #[error("frame exceeds the 1 MB native-messaging limit ({0} bytes)")]
    FrameTooLarge(usize),
    /// A frame failed to (de)serialize.
    #[error("failed to (de)serialize a frame: {0}")]
    Codec(String),
    /// The channel is closed.
    #[error("the transport channel is closed")]
    Closed,
    /// An I/O failure on the wire.
    #[error("transport i/o error: {0}")]
    Io(String),
}

/// Failures from the `SecretStore` port.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// The backing store is unavailable.
    #[error("secret backend is unavailable: {0}")]
    Backend(String),
    /// Access to the key was denied.
    #[error("secret store denied access to key {0:?}")]
    AccessDenied(SecretKey),
    /// The stored value was malformed.
    #[error("secret value was malformed: {0}")]
    Malformed(String),
}

/// Failures from the `Tier2Classifier` port.
#[derive(Debug, thiserror::Error)]
pub enum MlError {
    /// The feature vector was not valid for this model.
    #[error("feature vector was invalid for this model: {0}")]
    InvalidFeatures(String),
    /// The model is not loaded or not yet trained.
    #[error("model is not loaded or not yet trained")]
    NotReady,
    /// An ml-backend failure.
    #[error("ml backend error: {0}")]
    Backend(String),
}

/// Failures from the storage seam (`StorageBackend` and the repository ports).
///
/// The repositories return this — never a `rusqlite::Error`, a `Connection`, or any
/// driver type — so no backend detail crosses the port boundary. A concrete backend maps
/// its native failures onto these variants (e.g. a SQLite constraint code becomes
/// [`StorageError::Constraint`]).
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// A schema migration failed to apply.
    #[error("schema migration failed: {0}")]
    Migration(String),
    /// A constraint (foreign-key, uniqueness, NOT NULL) was violated.
    #[error("storage constraint violated: {0}")]
    Constraint(String),
    /// A stored value could not be (de)serialized to/from its column.
    #[error("storage (de)serialization error: {0}")]
    Serialization(String),
    /// The requested storage engine is not built into this binary.
    #[error("storage engine not supported: {0}")]
    UnsupportedEngine(String),
    /// An adapter/driver-level failure with no more specific variant.
    #[error("storage backend error: {0}")]
    Backend(String),
}

/// Aggregate error for call sites that prefer one type over per-port enums.
#[derive(Debug, thiserror::Error)]
pub enum MailMateError {
    /// A `MailClient` failure.
    #[error(transparent)]
    Mail(#[from] MailError),
    /// A `Transport` failure.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// A `SecretStore` failure.
    #[error(transparent)]
    Secret(#[from] SecretError),
    /// A `Tier2Classifier` failure.
    #[error(transparent)]
    Ml(#[from] MlError),
    /// A storage-seam failure.
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_error_displays_the_id() {
        let err = MailError::NotFound(MessageId::from("msg_404"));
        assert_eq!(err.to_string(), "message not found: msg_404");
    }

    #[test]
    fn leaf_errors_fold_into_the_aggregate() {
        let err: MailMateError = TransportError::Closed.into();
        assert!(matches!(
            err,
            MailMateError::Transport(TransportError::Closed)
        ));
        assert_eq!(err.to_string(), "the transport channel is closed");
    }

    #[test]
    fn secret_error_redacts_via_key_debug_only() {
        let err = SecretError::AccessDenied(SecretKey::from("ollama_api_key"));
        assert!(err.to_string().contains("ollama_api_key"));
    }

    #[test]
    fn storage_error_displays_and_folds_into_the_aggregate() {
        let err = StorageError::Constraint("FOREIGN KEY constraint failed".to_owned());
        assert!(err.to_string().contains("constraint"));
        let aggregate: MailMateError = StorageError::Migration("boom".to_owned()).into();
        assert!(matches!(
            aggregate,
            MailMateError::Storage(StorageError::Migration(_))
        ));
    }
}
