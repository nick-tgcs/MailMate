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

/// Failures from the `AiProvider` port and the task/validation layer above it.
#[derive(Clone, Debug, thiserror::Error)]
pub enum AiError {
    /// No provider is available (e.g. the registry is empty / not configured).
    #[error("ai provider unavailable: {0}")]
    Unavailable(String),
    /// The provider's transport/request failed.
    #[error("ai request failed ({code}): {message}")]
    RequestFailed {
        /// A short machine code (HTTP status, adapter code).
        code: String,
        /// A human-readable message.
        message: String,
    },
    /// The provider could not enforce the requested structured-output schema/grammar.
    #[error("structured-output enforcement failed: {0}")]
    SchemaEnforcement(String),
    /// A response failed structured validation (bad JSON, missing/extra fields, bad enum,
    /// out-of-range confidence). Such a response is audited and never drives an action.
    #[error("ai response failed validation: {0}")]
    Validation(String),
    /// A response could not be (de)serialized.
    #[error("ai (de)serialization error: {0}")]
    Codec(String),
}

/// Failures from the `RuleEngine` port.
#[derive(Debug, thiserror::Error)]
pub enum RuleEngineError {
    /// A rule condition referenced a field/operator combination that is not valid.
    #[error("invalid rule condition: {0}")]
    InvalidCondition(String),
    /// `explain` was asked about a decision the engine has no record of.
    #[error("no record of decision {0}")]
    UnknownDecision(String),
    /// An adapter-specific failure while evaluating rules.
    #[error("rule engine error: {0}")]
    Backend(String),
}

/// Failures from the `PolicyGuard` port.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    /// The policy context was missing a signal a policy needed to decide.
    #[error("policy context incomplete: {0}")]
    IncompleteContext(String),
    /// An adapter-specific failure while evaluating policy.
    #[error("policy guard error: {0}")]
    Backend(String),
}

/// Failures from the `ClassificationEngine` port (the cascade).
#[derive(Debug, thiserror::Error)]
pub enum ClassificationError {
    /// A Tier-1 classification-rule evaluation failed.
    #[error("classification rule evaluation failed: {0}")]
    Rules(String),
    /// The Tier-2 local model failed to predict.
    #[error("tier-2 model failed: {0}")]
    Model(String),
    /// The Tier-3 provider escalation failed (transport, enforcement, or validation).
    #[error("tier-3 provider escalation failed: {0}")]
    Provider(String),
}

impl From<RuleEngineError> for ClassificationError {
    fn from(value: RuleEngineError) -> Self {
        Self::Rules(value.to_string())
    }
}

impl From<MlError> for ClassificationError {
    fn from(value: MlError) -> Self {
        Self::Model(value.to_string())
    }
}

impl From<AiError> for ClassificationError {
    fn from(value: AiError) -> Self {
        Self::Provider(value.to_string())
    }
}

/// Failures from the `ActionPlanner` port (Pipeline 2 planning).
#[derive(Debug, thiserror::Error)]
pub enum ActionPlanningError {
    /// An action-rule evaluation failed.
    #[error("action rule evaluation failed: {0}")]
    Rules(String),
    /// Planning needs a stored message (an action targets a resolved message id), but the
    /// input message has not been persisted.
    #[error("planning requires a stored message, but the message has no resolved id")]
    MissingMessageId,
}

impl From<RuleEngineError> for ActionPlanningError {
    fn from(value: RuleEngineError) -> Self {
        Self::Rules(value.to_string())
    }
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
    /// A policy-guard failure.
    #[error(transparent)]
    Policy(#[from] PolicyError),
    /// A rule-engine failure.
    #[error(transparent)]
    Rule(#[from] RuleEngineError),
    /// An AI-provider failure.
    #[error(transparent)]
    Ai(#[from] AiError),
    /// A classification-engine (cascade) failure.
    #[error(transparent)]
    Classification(#[from] ClassificationError),
    /// An action-planning failure.
    #[error(transparent)]
    Planning(#[from] ActionPlanningError),
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
    fn classification_and_planning_errors_fold_from_leaf_errors() {
        let from_rules: ClassificationError =
            RuleEngineError::InvalidCondition("bad op".to_owned()).into();
        assert!(matches!(from_rules, ClassificationError::Rules(_)));
        let from_model: ClassificationError = MlError::NotReady.into();
        assert!(matches!(from_model, ClassificationError::Model(_)));
        let from_provider: ClassificationError = AiError::Unavailable("no model".to_owned()).into();
        assert!(matches!(from_provider, ClassificationError::Provider(_)));

        let planning: ActionPlanningError = RuleEngineError::Backend("boom".to_owned()).into();
        assert!(matches!(planning, ActionPlanningError::Rules(_)));
        let missing = ActionPlanningError::MissingMessageId;
        assert!(missing.to_string().contains("stored message"));

        let aggregate: MailMateError = ClassificationError::Model("x".to_owned()).into();
        assert!(matches!(
            aggregate,
            MailMateError::Classification(ClassificationError::Model(_))
        ));
        let aggregate: MailMateError = ActionPlanningError::MissingMessageId.into();
        assert!(matches!(
            aggregate,
            MailMateError::Planning(ActionPlanningError::MissingMessageId)
        ));
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
