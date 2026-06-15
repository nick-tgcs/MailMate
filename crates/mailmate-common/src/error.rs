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

/// Failures from the `LearningEngine` port and the learning adapter above the
/// repositories (feedback capture, evidence aggregation, proposal generation).
#[derive(Debug, thiserror::Error)]
pub enum LearningError {
    /// A storage-seam failure while reading/writing feedback, evidence, or proposals.
    #[error("learning storage error: {0}")]
    Storage(String),
    /// A rule-engine failure while back-testing or conflict-checking a candidate.
    #[error("learning rule-engine error: {0}")]
    Rules(String),
    /// A candidate proposal was malformed (e.g. not deterministically expressible).
    #[error("invalid proposal: {0}")]
    InvalidProposal(String),
}

impl From<StorageError> for LearningError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<RuleEngineError> for LearningError {
    fn from(value: RuleEngineError) -> Self {
        Self::Rules(value.to_string())
    }
}

/// Failures from the `RuleCurator` port and the curator adapter above the AI provider and
/// the rule/proposal stores.
#[derive(Debug, thiserror::Error)]
pub enum CuratorError {
    /// The AI provider failed or returned a response that did not validate. Such a response
    /// is audited and never drives a proposal.
    #[error("curator provider error: {0}")]
    Provider(String),
    /// A storage-seam failure while reading rules/feedback or persisting proposals/conflicts.
    #[error("curator storage error: {0}")]
    Storage(String),
    /// A rule-engine failure while detecting conflicts in a candidate or live rules.
    #[error("curator rule-engine error: {0}")]
    Rules(String),
}

impl From<AiError> for CuratorError {
    fn from(value: AiError) -> Self {
        Self::Provider(value.to_string())
    }
}

impl From<StorageError> for CuratorError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<RuleEngineError> for CuratorError {
    fn from(value: RuleEngineError) -> Self {
        Self::Rules(value.to_string())
    }
}

impl From<LearningError> for CuratorError {
    fn from(value: LearningError) -> Self {
        match value {
            LearningError::Rules(msg) => Self::Rules(msg),
            other => Self::Storage(other.to_string()),
        }
    }
}

/// Failures from the `ProposalReview` port — applying a human's accept/reject decision to a
/// proposal and (on acceptance) materializing its rule.
#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    /// No proposal with that id.
    #[error("proposal not found: {0}")]
    NotFound(String),
    /// The proposal was already reviewed (a terminal status) and cannot be re-decided.
    #[error("proposal already reviewed: {0}")]
    AlreadyReviewed(String),
    /// An acceptance asked to create a rule, but the proposal carries no rule draft.
    #[error("proposal has no rule draft to materialize: {0}")]
    MissingDraft(String),
    /// A storage-seam failure while reading the proposal or writing the rule/feedback.
    #[error("review storage error: {0}")]
    Storage(String),
}

impl From<StorageError> for ReviewError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value.to_string())
    }
}

/// Failures from deriving and rendering a training dataset on export.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// A privacy ceiling was violated: an example required a level the export forbids, and
    /// it could not be redacted down to the ceiling.
    #[error("export privacy violation: {0}")]
    Privacy(String),
    /// No example was eligible for the requested view (a dataset would be empty).
    #[error("no eligible examples for export: {0}")]
    Empty(String),
    /// A storage-seam failure while reading feedback or persisting a dataset record.
    #[error("export storage error: {0}")]
    Storage(String),
    /// A row could not be rendered to its serialized form.
    #[error("export serialization error: {0}")]
    Serialization(String),
}

impl From<StorageError> for ExportError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value.to_string())
    }
}

/// Failures from a `TrainerBackend` implementation (the swappable weight-crunching seam).
#[derive(Clone, Debug, thiserror::Error)]
pub enum TrainerError {
    /// The job asked for a capability the backend does not advertise (e.g. a LoRA job on a
    /// backend whose `capabilities().lora` is false). The honest-capabilities guard.
    #[error("trainer does not support this job: {0}")]
    Unsupported(String),
    /// The job itself was malformed (e.g. an empty dataset, an unknown objective).
    #[error("invalid training job: {0}")]
    InvalidJob(String),
    /// A backend-internal failure (compute error, external tool failure, …).
    #[error("trainer backend error: {0}")]
    Backend(String),
}

/// Failures from the training-pipeline orchestration (`TrainingPipeline` port): export →
/// train → import → evaluate → gate.
#[derive(Debug, thiserror::Error)]
pub enum TrainingError {
    /// The export stage failed.
    #[error("training export error: {0}")]
    Export(String),
    /// The trainer backend failed.
    #[error("training backend error: {0}")]
    Trainer(String),
    /// The evaluation stage failed.
    #[error("training evaluation error: {0}")]
    Evaluation(String),
    /// The candidate adapter was not metadata-compatible with the target base model.
    #[error("training compatibility error: {0}")]
    Compatibility(String),
    /// A storage-seam failure while persisting datasets/adapters/eval runs.
    #[error("training storage error: {0}")]
    Storage(String),
    /// An AI-provider failure during evaluation.
    #[error("training provider error: {0}")]
    Provider(String),
}

impl From<ExportError> for TrainingError {
    fn from(value: ExportError) -> Self {
        match value {
            ExportError::Storage(msg) => Self::Storage(msg),
            other => Self::Export(other.to_string()),
        }
    }
}

impl From<TrainerError> for TrainingError {
    fn from(value: TrainerError) -> Self {
        Self::Trainer(value.to_string())
    }
}

impl From<StorageError> for TrainingError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<AiError> for TrainingError {
    fn from(value: AiError) -> Self {
        Self::Provider(value.to_string())
    }
}

/// Failures from the follow-up workflow engine, scheduler, and exit detector
/// (`mailmate-workflow`). These drive the existing planner/drafter — they do not plan,
/// guard, or send themselves — so the taxonomy is storage, drafting, and FSM/state.
#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    /// A referenced workflow definition, version, instance, or pipeline item was absent.
    #[error("workflow entity not found: {0}")]
    NotFound(String),
    /// A storage-seam failure while reading/writing pipeline items, workflows, instances,
    /// or follow-up feedback.
    #[error("workflow storage error: {0}")]
    Storage(String),
    /// The follow-up drafter (the shared reply-drafter port) failed to produce a body.
    #[error("workflow drafting error: {0}")]
    Drafting(String),
    /// An instance was asked to make a transition its FSM does not permit, or its state
    /// violated the `next_due_at` non-NULL-iff-active invariant.
    #[error("invalid workflow state: {0}")]
    InvalidState(String),
}

impl From<StorageError> for WorkflowError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<AiError> for WorkflowError {
    fn from(value: AiError) -> Self {
        Self::Drafting(value.to_string())
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
    /// A learning-engine failure.
    #[error(transparent)]
    Learning(#[from] LearningError),
    /// An agent-curator failure.
    #[error(transparent)]
    Curator(#[from] CuratorError),
    /// A proposal-review failure.
    #[error(transparent)]
    Review(#[from] ReviewError),
    /// A training-dataset export failure.
    #[error(transparent)]
    Export(#[from] ExportError),
    /// A trainer-backend failure.
    #[error(transparent)]
    Trainer(#[from] TrainerError),
    /// A training-pipeline orchestration failure.
    #[error(transparent)]
    Training(#[from] TrainingError),
    /// A follow-up workflow / scheduler failure.
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
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

    #[test]
    fn learning_error_folds_from_leaf_errors_and_into_the_aggregate() {
        let from_storage: LearningError = StorageError::Backend("disk".to_owned()).into();
        assert!(matches!(from_storage, LearningError::Storage(_)));
        let from_rules: LearningError = RuleEngineError::UnknownDecision("dec_x".to_owned()).into();
        assert!(matches!(from_rules, LearningError::Rules(_)));
        let invalid = LearningError::InvalidProposal("not deterministic".to_owned());
        assert!(invalid.to_string().contains("invalid proposal"));
        let aggregate: MailMateError = LearningError::Storage("x".to_owned()).into();
        assert!(matches!(
            aggregate,
            MailMateError::Learning(LearningError::Storage(_))
        ));
    }

    #[test]
    fn curator_error_folds_from_leaf_errors_and_into_the_aggregate() {
        let from_provider: CuratorError = AiError::Unavailable("no model".to_owned()).into();
        assert!(matches!(from_provider, CuratorError::Provider(_)));
        let from_storage: CuratorError = StorageError::Backend("disk".to_owned()).into();
        assert!(matches!(from_storage, CuratorError::Storage(_)));
        let from_rules: CuratorError = RuleEngineError::InvalidCondition("bad".to_owned()).into();
        assert!(matches!(from_rules, CuratorError::Rules(_)));
        let aggregate: MailMateError = CuratorError::Provider("x".to_owned()).into();
        assert!(matches!(
            aggregate,
            MailMateError::Curator(CuratorError::Provider(_))
        ));
    }

    #[test]
    fn review_error_folds_and_reports_its_cases() {
        let not_found = ReviewError::NotFound("prop_404".to_owned());
        assert!(not_found.to_string().contains("prop_404"));
        let already = ReviewError::AlreadyReviewed("prop_1".to_owned());
        assert!(already.to_string().contains("already reviewed"));
        let missing = ReviewError::MissingDraft("prop_2".to_owned());
        assert!(missing.to_string().contains("no rule draft"));
        let from_storage: ReviewError = StorageError::Constraint("fk".to_owned()).into();
        assert!(matches!(from_storage, ReviewError::Storage(_)));
        let aggregate: MailMateError = ReviewError::NotFound("prop_x".to_owned()).into();
        assert!(matches!(
            aggregate,
            MailMateError::Review(ReviewError::NotFound(_))
        ));
    }

    #[test]
    fn training_errors_fold_from_leaf_errors_and_into_the_aggregate() {
        // Export folds storage to storage, everything else to export.
        let from_storage: ExportError = StorageError::Backend("disk".to_owned()).into();
        assert!(matches!(from_storage, ExportError::Storage(_)));
        let privacy = ExportError::Privacy("body above ceiling".to_owned());
        assert!(privacy.to_string().contains("privacy"));

        // TrainerError reports the honest-capabilities case.
        let unsupported = TrainerError::Unsupported("lora not advertised".to_owned());
        assert!(unsupported.to_string().contains("does not support"));

        // TrainingError folds from each stage's leaf error.
        let from_export: TrainingError = ExportError::Empty("no rows".to_owned()).into();
        assert!(matches!(from_export, TrainingError::Export(_)));
        let from_export_storage: TrainingError = ExportError::Storage("disk".to_owned()).into();
        assert!(matches!(from_export_storage, TrainingError::Storage(_)));
        let from_trainer: TrainingError = TrainerError::Backend("compute".to_owned()).into();
        assert!(matches!(from_trainer, TrainingError::Trainer(_)));
        let from_storage: TrainingError = StorageError::Constraint("fk".to_owned()).into();
        assert!(matches!(from_storage, TrainingError::Storage(_)));
        let from_provider: TrainingError = AiError::Unavailable("no model".to_owned()).into();
        assert!(matches!(from_provider, TrainingError::Provider(_)));

        let aggregate: MailMateError = ExportError::Empty("x".to_owned()).into();
        assert!(matches!(
            aggregate,
            MailMateError::Export(ExportError::Empty(_))
        ));
        let aggregate: MailMateError = TrainerError::InvalidJob("x".to_owned()).into();
        assert!(matches!(
            aggregate,
            MailMateError::Trainer(TrainerError::InvalidJob(_))
        ));
        let aggregate: MailMateError = TrainingError::Evaluation("x".to_owned()).into();
        assert!(matches!(
            aggregate,
            MailMateError::Training(TrainingError::Evaluation(_))
        ));
    }

    #[test]
    fn workflow_error_folds_storage_and_ai_and_aggregates() {
        let from_storage: WorkflowError = StorageError::Constraint("fk".to_owned()).into();
        assert!(matches!(from_storage, WorkflowError::Storage(_)));
        let from_ai: WorkflowError = AiError::Unavailable("no provider".to_owned()).into();
        assert!(matches!(from_ai, WorkflowError::Drafting(_)));
        let aggregate: MailMateError = WorkflowError::NotFound("wfi_x".to_owned()).into();
        assert!(matches!(
            aggregate,
            MailMateError::Workflow(WorkflowError::NotFound(_))
        ));
    }
}
