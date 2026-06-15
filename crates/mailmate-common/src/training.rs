//! The on-device training-layer vocabulary — the **derived, provider-neutral** training
//! example and the dataset / adapter / evaluation records the export pipeline produces.
//!
//! Two structural rules from the architecture shape these types:
//!
//! 1. **There is no `training_examples` table.** A [`TrainingExample`] is *derived on
//!    export* from a per-task feedback row (the single source of truth); its `id` is an
//!    ephemeral export id, never persisted. Only the [`TrainingDatasetRecord`],
//!    [`LoraAdapterRecord`], and [`LoraEvalRunRecord`] are durable.
//! 2. **A LoRA adapter is advisory.** These types describe data and metadata only; nothing
//!    here can activate a rule or weaken a policy. An adapter reaches
//!    [`AdapterStatus::Active`] only after an evaluation gate (see `evaluate_promotion` in
//!    `mailmate-training`), and even then its outputs still flow through validation → rules
//!    → policy like any other provider's.
//!
//! Every enum persisted to a `TEXT` column carries the project-standard `as_str` /
//! `from_db_str` pair so storage never depends on serde's string shape.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::evidence::EvidenceSourceKind;
use crate::feedback::FeedbackPolarity;
use crate::ids::{AdapterId, DatasetId, EvalRunId, FeedbackId};
use crate::time::Timestamp;

/// The AI function a training signal came from — the *signal taxonomy* from the spec. One
/// per per-task feedback source, plus the cross-cutting [`TrainingTask::Safety`] signal
/// derived from policy blocks and unsafe-flagged outputs.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrainingTask {
    /// Junk / phishing / priority / labels.
    Classification,
    /// Folder suggestion / move.
    Filing,
    /// Reply generation.
    DraftReply,
    /// Thread summaries.
    Summary,
    /// Extracted tasks.
    TaskExtraction,
    /// Curator proposal wording / granularity.
    RuleProposal,
    /// Safety counterexamples — forbidden behaviour the model must learn to avoid.
    Safety,
}

impl TrainingTask {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Classification => "classification",
            Self::Filing => "filing",
            Self::DraftReply => "draft_reply",
            Self::Summary => "summary",
            Self::TaskExtraction => "task_extraction",
            Self::RuleProposal => "rule_proposal",
            Self::Safety => "safety",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "classification" => Some(Self::Classification),
            "filing" => Some(Self::Filing),
            "draft_reply" => Some(Self::DraftReply),
            "summary" => Some(Self::Summary),
            "task_extraction" => Some(Self::TaskExtraction),
            "rule_proposal" => Some(Self::RuleProposal),
            "safety" => Some(Self::Safety),
            _ => None,
        }
    }

    /// The task a feedback source maps to, when one captures training signal for a task.
    /// `RuleProposal`/`FollowUp` and the message tasks map across; there is no 1:1 task for
    /// every source, so this returns an `Option`.
    #[must_use]
    pub fn from_source_kind(source: EvidenceSourceKind) -> Option<Self> {
        match source {
            EvidenceSourceKind::Classification => Some(Self::Classification),
            EvidenceSourceKind::Filing => Some(Self::Filing),
            EvidenceSourceKind::Draft => Some(Self::DraftReply),
            EvidenceSourceKind::Summary => Some(Self::Summary),
            EvidenceSourceKind::TaskExtraction => Some(Self::TaskExtraction),
            EvidenceSourceKind::RuleProposal => Some(Self::RuleProposal),
            EvidenceSourceKind::FollowUp => None,
        }
    }
}

/// The outcome label on a derived example — richer than bare polarity so SFT, preference,
/// and evaluation views can select the right rows.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrainingLabel {
    /// User accepted the output as-is.
    Accepted,
    /// User made small edits before accepting.
    AcceptedWithMinorEdits,
    /// User fixed the output.
    Corrected,
    /// User rejected the output.
    Rejected,
    /// User discarded a draft / summary / task.
    Discarded,
    /// User undid the action.
    Undone,
    /// Policy rejected the output.
    BlockedByPolicy,
    /// Output violated a safety constraint.
    Unsafe,
    /// Demonstrates when *not* to apply a pattern.
    Counterexample,
}

impl TrainingLabel {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::AcceptedWithMinorEdits => "accepted_with_minor_edits",
            Self::Corrected => "corrected",
            Self::Rejected => "rejected",
            Self::Discarded => "discarded",
            Self::Undone => "undone",
            Self::BlockedByPolicy => "blocked_by_policy",
            Self::Unsafe => "unsafe",
            Self::Counterexample => "counterexample",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "accepted" => Some(Self::Accepted),
            "accepted_with_minor_edits" => Some(Self::AcceptedWithMinorEdits),
            "corrected" => Some(Self::Corrected),
            "rejected" => Some(Self::Rejected),
            "discarded" => Some(Self::Discarded),
            "undone" => Some(Self::Undone),
            "blocked_by_policy" => Some(Self::BlockedByPolicy),
            "unsafe" => Some(Self::Unsafe),
            "counterexample" => Some(Self::Counterexample),
            _ => None,
        }
    }

    /// Whether this label is a *positive* training signal (the model should imitate it).
    #[must_use]
    pub fn is_positive(self) -> bool {
        matches!(
            self,
            Self::Accepted | Self::AcceptedWithMinorEdits | Self::Corrected
        )
    }

    /// Whether this label marks forbidden / unsafe behaviour the model must learn to avoid.
    /// Such examples are eligible for the safety-counterexample view, never for SFT.
    #[must_use]
    pub fn is_safety_negative(self) -> bool {
        matches!(
            self,
            Self::BlockedByPolicy | Self::Unsafe | Self::Counterexample
        )
    }

    /// The polarity this label implies.
    #[must_use]
    pub fn polarity(self) -> FeedbackPolarity {
        if self.is_positive() {
            FeedbackPolarity::Positive
        } else {
            FeedbackPolarity::Negative
        }
    }
}

/// The privacy classification of a derived example's *content*, in increasing sensitivity.
///
/// This is distinct from [`RetentionLevel`](crate::retention::RetentionLevel) (what the
/// user consented to *store*): it classifies how much real content an example carries after
/// redaction, so an export can refuse to emit anything above a requested ceiling.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportPrivacyLevel {
    /// Features and labels only — no readable body text. The safe default.
    #[default]
    Metadata,
    /// Body text with PII scrubbed (emails, phone numbers, payment-shaped tokens removed).
    Redacted,
    /// Raw, unredacted body text. Only ever produced under an explicit opt-in.
    Full,
}

impl ExportPrivacyLevel {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Redacted => "redacted",
            Self::Full => "full",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "metadata" => Some(Self::Metadata),
            "redacted" => Some(Self::Redacted),
            "full" => Some(Self::Full),
            _ => None,
        }
    }
}

/// A deterministically-detectable category of forbidden commitment / unsafe content. The
/// set is closed (no free-text `Other`) so safety detection stays auditable and the column
/// round-trips cleanly.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyFlag {
    /// A change to payment / banking details.
    PaymentChange,
    /// A commitment to a price or amount.
    PriceCommitment,
    /// A commitment to a date or deadline.
    DateCommitment,
    /// A legal position or admission.
    LegalPosition,
    /// Disclosure of a secret, credential, or token.
    CredentialDisclosure,
    /// A commitment the source thread does not support.
    UnsupportedCommitment,
}

impl SafetyFlag {
    /// Every flag, for exhaustive iteration in tests and detectors.
    #[must_use]
    pub fn all() -> [Self; 6] {
        [
            Self::PaymentChange,
            Self::PriceCommitment,
            Self::DateCommitment,
            Self::LegalPosition,
            Self::CredentialDisclosure,
            Self::UnsupportedCommitment,
        ]
    }

    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PaymentChange => "payment_change",
            Self::PriceCommitment => "price_commitment",
            Self::DateCommitment => "date_commitment",
            Self::LegalPosition => "legal_position",
            Self::CredentialDisclosure => "credential_disclosure",
            Self::UnsupportedCommitment => "unsupported_commitment",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        Self::all().into_iter().find(|f| f.as_str() == s)
    }
}

/// Which dataset view a [`TrainingDatasetRecord`] holds.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetType {
    /// Supervised fine-tuning: instruction + redacted context → accepted/corrected target.
    Sft,
    /// Preference pairs: chosen vs rejected output.
    Preference,
    /// Frozen test cases never used for training.
    Evaluation,
    /// Negative examples of forbidden behaviour.
    SafetyCounterexample,
}

impl DatasetType {
    /// Every dataset type.
    #[must_use]
    pub fn all() -> [Self; 4] {
        [
            Self::Sft,
            Self::Preference,
            Self::Evaluation,
            Self::SafetyCounterexample,
        ]
    }

    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sft => "sft",
            Self::Preference => "preference",
            Self::Evaluation => "evaluation",
            Self::SafetyCounterexample => "safety_counterexample",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "sft" => Some(Self::Sft),
            "preference" => Some(Self::Preference),
            "evaluation" => Some(Self::Evaluation),
            "safety_counterexample" => Some(Self::SafetyCounterexample),
            _ => None,
        }
    }
}

/// The on-disk serialization shape an export renders to.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// Chat-style JSONL: a `messages` array plus a target field.
    JsonlChat,
    /// Alpaca-style JSONL: `instruction` / `input` / `output`.
    Alpaca,
    /// Preference JSONL: `messages` + `chosen` + `rejected`.
    PreferenceJsonl,
}

impl ExportFormat {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::JsonlChat => "jsonl_chat",
            Self::Alpaca => "alpaca",
            Self::PreferenceJsonl => "preference_jsonl",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "jsonl_chat" => Some(Self::JsonlChat),
            "alpaca" => Some(Self::Alpaca),
            "preference_jsonl" => Some(Self::PreferenceJsonl),
            _ => None,
        }
    }

    /// The format a dataset type renders to by default.
    #[must_use]
    pub fn default_for(dataset_type: DatasetType) -> Self {
        match dataset_type {
            DatasetType::Preference => Self::PreferenceJsonl,
            _ => Self::JsonlChat,
        }
    }
}

/// The train/validation/test/holdout partition an example lands in. The partition is a
/// deterministic function of the example's source feedback id (see `mailmate-training`),
/// not a stored column.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetSplit {
    /// Used to fit the model.
    Train,
    /// Used to tune / early-stop.
    Validation,
    /// Held out to measure generalization.
    Test,
    /// Never touched by training or tuning; reserved for final audit.
    Holdout,
}

impl DatasetSplit {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Train => "train",
            Self::Validation => "validation",
            Self::Test => "test",
            Self::Holdout => "holdout",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "train" => Some(Self::Train),
            "validation" => Some(Self::Validation),
            "test" => Some(Self::Test),
            "holdout" => Some(Self::Holdout),
            _ => None,
        }
    }
}

/// Which feedback row a derived example came from — a typed back-pointer at the single
/// source of truth, mirroring `rule_evidence`'s `(source_kind, source_id)` shape.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceFeedbackRef {
    /// Which per-task feedback table the row lives in.
    pub kind: EvidenceSourceKind,
    /// The id of the source feedback row.
    pub id: FeedbackId,
}

/// Provider-neutral, redacted context for an example. Task-agnostic so every deriver fills
/// the same shape.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ContextFeatures {
    /// The sender's domain, when relevant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_domain: Option<String>,
    /// A short, redacted thread summary, when the retention level permits one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_summary: Option<String>,
    /// Commitments the model must never make for this example (`dates`, `prices`, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_commitments: Vec<String>,
    /// Other deterministic, non-sensitive features (sorted, so JSON is stable).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
}

/// The instruction half of a derived example.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TrainingInput {
    /// The system framing, when one applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// The task instruction.
    pub instruction: String,
    /// Redacted context features.
    pub context_features: ContextFeatures,
}

/// A candidate or corrected output body.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CandidateOutput {
    /// The output text (a draft body, a chosen label, a summary, …).
    pub body: String,
}

impl CandidateOutput {
    /// Wrap a body string.
    #[must_use]
    pub fn new(body: impl Into<String>) -> Self {
        Self { body: body.into() }
    }
}

/// A single derived training example — **never persisted**, built on export from one
/// feedback row. Its `id` is an ephemeral export identity (`trn_…`), stable for a given
/// source row so exports are reproducible.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TrainingExample {
    /// Ephemeral, reproducible export id (`trn_<hash-of-source-id>`).
    pub id: String,
    /// The AI function this example trains.
    pub task: TrainingTask,
    /// The feedback row it was derived from.
    pub source_feedback: SourceFeedbackRef,
    /// The privacy classification of this example's content.
    pub privacy_level: ExportPrivacyLevel,
    /// The intended base-model family, when the export targets one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_model_family: Option<String>,
    /// Instruction + redacted context.
    pub input: TrainingInput,
    /// What the AI produced, when there is a candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_output: Option<CandidateOutput>,
    /// What the human corrected it to, when corrected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_corrected_output: Option<CandidateOutput>,
    /// The outcome label.
    pub label: TrainingLabel,
    /// The polarity (kept alongside the label for fast view selection).
    pub polarity: FeedbackPolarity,
    /// A 0.0–1.0 quality score (higher = stronger positive signal).
    pub quality_score: f64,
    /// Any safety flags the content tripped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safety_flags: Vec<SafetyFlag>,
    /// When the source feedback was captured.
    pub created_at: Timestamp,
}

impl TrainingExample {
    /// The output a positive/SFT view should train toward: the human correction if present,
    /// else the accepted candidate. `None` when neither is available (a pure rejection).
    #[must_use]
    pub fn target_output(&self) -> Option<&CandidateOutput> {
        self.user_corrected_output
            .as_ref()
            .or(self.candidate_output.as_ref())
    }

    /// Whether this example is eligible for the supervised fine-tuning view: a positive
    /// label, a usable target, and no safety flags.
    #[must_use]
    pub fn is_sft_eligible(&self) -> bool {
        self.label.is_positive() && self.target_output().is_some() && self.safety_flags.is_empty()
    }
}

/// A durable dataset record (`training_datasets`). The examples themselves are not stored;
/// `example_ids_hash` pins the exact set that produced this dataset.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TrainingDatasetRecord {
    /// Dataset id (`ds_…`).
    pub id: DatasetId,
    /// Human-readable name.
    pub name: String,
    /// Which view this dataset holds.
    pub dataset_type: DatasetType,
    /// Intended base-model family, if any.
    pub base_model_family: Option<String>,
    /// Stable hash of the included source ids (the dataset's content identity).
    pub example_ids_hash: String,
    /// Count of positive examples.
    pub positive_count: usize,
    /// Count of negative examples.
    pub negative_count: usize,
    /// Count of validation examples.
    pub validation_count: usize,
    /// Count of held-out test examples.
    pub test_count: usize,
    /// The highest privacy level any included example carries.
    pub privacy_level: ExportPrivacyLevel,
    /// The serialization format the artifact was rendered in.
    pub export_format: ExportFormat,
    /// Where the rendered artifact was written, if anywhere.
    pub artifact_path: Option<String>,
    /// When the dataset was built.
    pub created_at: Timestamp,
}

/// The kind of adapter an artifact is.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterType {
    /// A low-rank adapter.
    Lora,
    /// A quantized low-rank adapter.
    Qlora,
}

impl AdapterType {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lora => "lora",
            Self::Qlora => "qlora",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "lora" => Some(Self::Lora),
            "qlora" => Some(Self::Qlora),
            _ => None,
        }
    }
}

/// The on-disk weight format of an adapter artifact.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterFormat {
    /// `safetensors`.
    Safetensors,
    /// A PyTorch `.pt` / `.bin` checkpoint.
    Pytorch,
}

impl AdapterFormat {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Safetensors => "safetensors",
            Self::Pytorch => "pytorch",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "safetensors" => Some(Self::Safetensors),
            "pytorch" => Some(Self::Pytorch),
            _ => None,
        }
    }
}

/// The lifecycle status of a registered adapter. An adapter never starts `Active`; it is
/// promoted there only by passing the evaluation gate, and demoted to `FailedEval` if it
/// regresses safety or quality.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterStatus {
    /// Registered, not yet evaluated/approved.
    Candidate,
    /// Passed the gate and approved for use.
    Active,
    /// Superseded / withdrawn.
    Retired,
    /// Failed the evaluation gate.
    FailedEval,
}

impl AdapterStatus {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Active => "active",
            Self::Retired => "retired",
            Self::FailedEval => "failed_eval",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "candidate" => Some(Self::Candidate),
            "active" => Some(Self::Active),
            "retired" => Some(Self::Retired),
            "failed_eval" => Some(Self::FailedEval),
            _ => None,
        }
    }

    /// Whether an adapter at this status may serve.
    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// A durable adapter record (`lora_adapters`). `training_dataset_id` is optional so an
/// externally-trained adapter imported by metadata (with no local dataset) can still be
/// registered.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LoraAdapterRecord {
    /// Adapter id (`lora_…`).
    pub id: AdapterId,
    /// Adapter name.
    pub name: String,
    /// The adapter kind.
    pub adapter_type: AdapterType,
    /// The weight format.
    pub format: AdapterFormat,
    /// Compatible base-model family.
    pub base_model_family: String,
    /// The exact base model it was trained against.
    pub base_model_name: String,
    /// Base model revision / hash, when known.
    pub base_model_revision: Option<String>,
    /// Tokenizer compatibility hash, when known.
    pub tokenizer_hash: Option<String>,
    /// Chat-template compatibility hash, when known.
    pub chat_template_hash: Option<String>,
    /// The dataset it was trained on, when locally exported.
    pub training_dataset_id: Option<DatasetId>,
    /// Where the artifact lives.
    pub artifact_path: String,
    /// Lifecycle status.
    pub status: AdapterStatus,
    /// When registered.
    pub created_at: Timestamp,
}

/// Aggregate evaluation metrics for an adapter run, serialized whole into `metrics_json`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct EvalMetrics {
    /// Task accuracy, when measurable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accuracy: Option<f64>,
    /// Preference win-rate against the base provider, when measurable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub win_rate: Option<f64>,
    /// Count of safety regressions observed.
    pub safety_failures: usize,
    /// Aggregate 0.0–1.0 quality score.
    pub quality_score: f64,
    /// Any additional named metrics (sorted, so JSON is stable).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, f64>,
}

/// A durable evaluation run (`lora_eval_runs`).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LoraEvalRunRecord {
    /// Eval-run id (`eval_…`).
    pub id: EvalRunId,
    /// The adapter under evaluation.
    pub adapter_id: AdapterId,
    /// The evaluation dataset used.
    pub dataset_id: DatasetId,
    /// The base provider the adapter was layered onto.
    pub base_provider_id: String,
    /// The full metrics.
    pub metrics: EvalMetrics,
    /// Count of safety regressions (mirrors `metrics.safety_failures` for cheap querying).
    pub safety_failures: usize,
    /// Aggregate quality score (mirrors `metrics.quality_score`).
    pub quality_score: f64,
    /// Whether the run was approved for use (the gate's verdict).
    pub approved_for_use: bool,
    /// When the run completed.
    pub created_at: Timestamp,
}

/// The thresholds an adapter must clear before it can be promoted to `Active`. This is the
/// crystallization gate's policy: an adapter that does not clear *every* bar stays a
/// candidate, exactly as a learned rule must clear precision + support before activation.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct PromotionPolicy {
    /// Minimum aggregate quality score (inclusive).
    pub min_quality_score: f64,
    /// Maximum tolerated safety regressions (inclusive).
    pub max_safety_failures: usize,
    /// Whether the adapter must be metadata-compatible with the target base model.
    pub require_compatible: bool,
}

impl Default for PromotionPolicy {
    /// A conservative default: a solidly-positive quality score and **zero** tolerated
    /// safety failures — a single safety regression blocks promotion.
    fn default() -> Self {
        Self {
            min_quality_score: 0.7,
            max_safety_failures: 0,
            require_compatible: true,
        }
    }
}

/// The gate's verdict.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum PromotionDecision {
    /// Clear to promote to `Active`.
    Promote,
    /// Blocked, with the reasons it failed.
    Reject {
        /// One human-readable reason per failed bar.
        reasons: Vec<String>,
    },
}

impl PromotionDecision {
    /// Whether the decision is to promote.
    #[must_use]
    pub fn is_promote(&self) -> bool {
        matches!(self, Self::Promote)
    }

    /// The status an adapter takes after this decision.
    #[must_use]
    pub fn resulting_status(&self) -> AdapterStatus {
        if self.is_promote() {
            AdapterStatus::Active
        } else {
            AdapterStatus::FailedEval
        }
    }
}

/// What a training run optimizes. Distinct from [`DatasetType`] (a *view* of the data):
/// the objective is what the trainer does with it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrainingObjective {
    /// Supervised fine-tuning on positive/corrected targets.
    Sft,
    /// Preference optimization on chosen-vs-rejected pairs.
    Preference,
}

impl TrainingObjective {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sft => "sft",
            Self::Preference => "preference",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "sft" => Some(Self::Sft),
            "preference" => Some(Self::Preference),
            _ => None,
        }
    }

    /// The training-dataset view this objective consumes.
    #[must_use]
    pub fn dataset_type(self) -> DatasetType {
        match self {
            Self::Sft => DatasetType::Sft,
            Self::Preference => DatasetType::Preference,
        }
    }
}

/// A request to run the whole training pipeline: derive a dataset from the captured
/// feedback for the named tasks, train a candidate adapter, evaluate it, and gate its
/// promotion. The pipeline never activates anything that fails the gate.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TrainingPipelineRequest {
    /// A human-readable name for the produced dataset(s).
    pub dataset_name: String,
    /// Which task signals to include (empty = every captured task).
    #[serde(default)]
    pub tasks: Vec<TrainingTask>,
    /// The base-model family to target. Required for a LoRA run (an adapter is only portable
    /// against a known family); `None` is rejected by the pipeline.
    #[serde(default)]
    pub base_model_family: Option<String>,
    /// The exact base model the adapter is trained against, for honest compatibility
    /// metadata. Defaults to the family when unspecified.
    #[serde(default)]
    pub base_model_name: Option<String>,
    /// The highest privacy level the export may emit; content above this is redacted down
    /// or dropped.
    pub privacy_ceiling: ExportPrivacyLevel,
    /// What the run optimizes.
    pub objective: TrainingObjective,
    /// The provider id the candidate is evaluated against.
    pub base_provider_id: String,
    /// The thresholds the candidate must clear to be promoted to `Active`.
    pub promotion_policy: PromotionPolicy,
}

impl TrainingPipelineRequest {
    /// A request over every captured task with conservative, privacy-safe defaults: a
    /// `Redacted` ceiling, the SFT objective, and the default promotion policy.
    #[must_use]
    pub fn new(dataset_name: impl Into<String>, base_provider_id: impl Into<String>) -> Self {
        Self {
            dataset_name: dataset_name.into(),
            tasks: Vec::new(),
            base_model_family: None,
            base_model_name: None,
            privacy_ceiling: ExportPrivacyLevel::Redacted,
            objective: TrainingObjective::Sft,
            base_provider_id: base_provider_id.into(),
            promotion_policy: PromotionPolicy::default(),
        }
    }

    /// Whether a task is included by this request (an empty `tasks` includes everything).
    #[must_use]
    pub fn includes(&self, task: TrainingTask) -> bool {
        self.tasks.is_empty() || self.tasks.contains(&task)
    }
}

/// Everything one pipeline run produced — the durable records plus the gate's verdict. The
/// `adapter.status` already reflects `promotion` (Active on promote, FailedEval on reject).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TrainingPipelineReport {
    /// The training dataset the candidate was trained on.
    pub dataset: TrainingDatasetRecord,
    /// The held-out evaluation dataset.
    pub eval_dataset: TrainingDatasetRecord,
    /// The candidate adapter, with its post-gate status.
    pub adapter: LoraAdapterRecord,
    /// The evaluation run that fed the gate.
    pub eval_run: LoraEvalRunRecord,
    /// The gate's verdict.
    pub promotion: PromotionDecision,
}

impl TrainingPipelineReport {
    /// Whether the run ended with an adapter promoted to `Active`.
    #[must_use]
    pub fn promoted(&self) -> bool {
        self.promotion.is_promote() && self.adapter.status.is_active()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn training_objective_round_trips_and_maps_to_a_view() {
        for obj in [TrainingObjective::Sft, TrainingObjective::Preference] {
            assert_eq!(TrainingObjective::from_db_str(obj.as_str()), Some(obj));
        }
        assert_eq!(TrainingObjective::from_db_str("nope"), None);
        assert_eq!(TrainingObjective::Sft.dataset_type(), DatasetType::Sft);
        assert_eq!(
            TrainingObjective::Preference.dataset_type(),
            DatasetType::Preference
        );
    }

    #[test]
    fn pipeline_request_defaults_are_privacy_safe_and_include_everything() {
        let req = TrainingPipelineRequest::new("nightly", "prov_mock");
        assert_eq!(req.privacy_ceiling, ExportPrivacyLevel::Redacted);
        assert_eq!(req.objective, TrainingObjective::Sft);
        assert!(
            req.includes(TrainingTask::DraftReply),
            "empty tasks includes all"
        );
        let focused = TrainingPipelineRequest {
            tasks: vec![TrainingTask::Classification],
            ..TrainingPipelineRequest::new("c", "p")
        };
        assert!(focused.includes(TrainingTask::Classification));
        assert!(!focused.includes(TrainingTask::DraftReply));
    }

    #[test]
    fn task_labels_round_trip_and_map_from_sources() {
        for task in [
            TrainingTask::Classification,
            TrainingTask::Filing,
            TrainingTask::DraftReply,
            TrainingTask::Summary,
            TrainingTask::TaskExtraction,
            TrainingTask::RuleProposal,
            TrainingTask::Safety,
        ] {
            assert_eq!(TrainingTask::from_db_str(task.as_str()), Some(task));
        }
        assert_eq!(TrainingTask::from_db_str("nope"), None);
        assert_eq!(
            TrainingTask::from_source_kind(EvidenceSourceKind::Classification),
            Some(TrainingTask::Classification)
        );
        assert_eq!(
            TrainingTask::from_source_kind(EvidenceSourceKind::Draft),
            Some(TrainingTask::DraftReply)
        );
        assert_eq!(
            TrainingTask::from_source_kind(EvidenceSourceKind::FollowUp),
            None
        );
    }

    #[test]
    fn training_label_round_trips_and_classifies_polarity_and_safety() {
        for label in [
            TrainingLabel::Accepted,
            TrainingLabel::AcceptedWithMinorEdits,
            TrainingLabel::Corrected,
            TrainingLabel::Rejected,
            TrainingLabel::Discarded,
            TrainingLabel::Undone,
            TrainingLabel::BlockedByPolicy,
            TrainingLabel::Unsafe,
            TrainingLabel::Counterexample,
        ] {
            assert_eq!(TrainingLabel::from_db_str(label.as_str()), Some(label));
        }
        assert_eq!(TrainingLabel::from_db_str("nope"), None);

        assert!(TrainingLabel::Accepted.is_positive());
        assert!(TrainingLabel::Corrected.is_positive());
        assert!(!TrainingLabel::Rejected.is_positive());
        assert_eq!(
            TrainingLabel::Accepted.polarity(),
            FeedbackPolarity::Positive
        );
        assert_eq!(
            TrainingLabel::Rejected.polarity(),
            FeedbackPolarity::Negative
        );

        assert!(TrainingLabel::Unsafe.is_safety_negative());
        assert!(TrainingLabel::BlockedByPolicy.is_safety_negative());
        assert!(TrainingLabel::Counterexample.is_safety_negative());
        assert!(!TrainingLabel::Rejected.is_safety_negative());
    }

    #[test]
    fn privacy_level_is_ordered_and_round_trips() {
        assert!(ExportPrivacyLevel::Metadata < ExportPrivacyLevel::Redacted);
        assert!(ExportPrivacyLevel::Redacted < ExportPrivacyLevel::Full);
        assert_eq!(ExportPrivacyLevel::default(), ExportPrivacyLevel::Metadata);
        for level in [
            ExportPrivacyLevel::Metadata,
            ExportPrivacyLevel::Redacted,
            ExportPrivacyLevel::Full,
        ] {
            assert_eq!(ExportPrivacyLevel::from_db_str(level.as_str()), Some(level));
        }
        assert_eq!(ExportPrivacyLevel::from_db_str("nope"), None);
    }

    #[test]
    fn safety_flag_round_trips_every_variant() {
        for flag in SafetyFlag::all() {
            assert_eq!(SafetyFlag::from_db_str(flag.as_str()), Some(flag));
        }
        assert_eq!(SafetyFlag::from_db_str("nope"), None);
        assert_eq!(SafetyFlag::all().len(), 6);
    }

    #[test]
    fn dataset_and_export_and_split_labels_round_trip() {
        for dt in DatasetType::all() {
            assert_eq!(DatasetType::from_db_str(dt.as_str()), Some(dt));
        }
        assert_eq!(DatasetType::from_db_str("nope"), None);
        assert_eq!(
            ExportFormat::default_for(DatasetType::Preference),
            ExportFormat::PreferenceJsonl
        );
        assert_eq!(
            ExportFormat::default_for(DatasetType::Sft),
            ExportFormat::JsonlChat
        );
        for ef in [
            ExportFormat::JsonlChat,
            ExportFormat::Alpaca,
            ExportFormat::PreferenceJsonl,
        ] {
            assert_eq!(ExportFormat::from_db_str(ef.as_str()), Some(ef));
        }
        assert_eq!(ExportFormat::from_db_str("nope"), None);
        for split in [
            DatasetSplit::Train,
            DatasetSplit::Validation,
            DatasetSplit::Test,
            DatasetSplit::Holdout,
        ] {
            assert_eq!(DatasetSplit::from_db_str(split.as_str()), Some(split));
        }
        assert_eq!(DatasetSplit::from_db_str("nope"), None);
    }

    #[test]
    fn adapter_enums_round_trip_and_only_active_serves() {
        for at in [AdapterType::Lora, AdapterType::Qlora] {
            assert_eq!(AdapterType::from_db_str(at.as_str()), Some(at));
        }
        assert_eq!(AdapterType::from_db_str("nope"), None);
        for af in [AdapterFormat::Safetensors, AdapterFormat::Pytorch] {
            assert_eq!(AdapterFormat::from_db_str(af.as_str()), Some(af));
        }
        assert_eq!(AdapterFormat::from_db_str("nope"), None);
        for st in [
            AdapterStatus::Candidate,
            AdapterStatus::Active,
            AdapterStatus::Retired,
            AdapterStatus::FailedEval,
        ] {
            assert_eq!(AdapterStatus::from_db_str(st.as_str()), Some(st));
        }
        assert_eq!(AdapterStatus::from_db_str("nope"), None);
        assert!(AdapterStatus::Active.is_active());
        assert!(!AdapterStatus::Candidate.is_active());
    }

    fn an_example(
        label: TrainingLabel,
        corrected: Option<&str>,
        candidate: Option<&str>,
    ) -> TrainingExample {
        TrainingExample {
            id: "trn_x".to_owned(),
            task: TrainingTask::DraftReply,
            source_feedback: SourceFeedbackRef {
                kind: EvidenceSourceKind::Draft,
                id: FeedbackId::from("drffb_1"),
            },
            privacy_level: ExportPrivacyLevel::Redacted,
            base_model_family: Some("llama".to_owned()),
            input: TrainingInput {
                system: Some("You are MailMate".to_owned()),
                instruction: "Draft a reply".to_owned(),
                context_features: ContextFeatures::default(),
            },
            candidate_output: candidate.map(CandidateOutput::new),
            user_corrected_output: corrected.map(CandidateOutput::new),
            label,
            polarity: label.polarity(),
            quality_score: 0.8,
            safety_flags: vec![],
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn target_output_prefers_the_human_correction() {
        let ex = an_example(TrainingLabel::Corrected, Some("fixed"), Some("ai"));
        assert_eq!(ex.target_output().unwrap().body, "fixed");
        let ex = an_example(TrainingLabel::Accepted, None, Some("ai"));
        assert_eq!(ex.target_output().unwrap().body, "ai");
        let ex = an_example(TrainingLabel::Rejected, None, None);
        assert!(ex.target_output().is_none());
    }

    #[test]
    fn sft_eligibility_requires_positive_target_and_no_safety_flags() {
        assert!(an_example(TrainingLabel::Accepted, None, Some("ai")).is_sft_eligible());
        // A rejection has no positive signal.
        assert!(!an_example(TrainingLabel::Rejected, None, Some("ai")).is_sft_eligible());
        // A positive label with a safety flag is excluded.
        let mut flagged = an_example(TrainingLabel::Accepted, None, Some("ai"));
        flagged.safety_flags.push(SafetyFlag::PaymentChange);
        assert!(!flagged.is_sft_eligible());
        // A positive label with no usable target is excluded.
        assert!(!an_example(TrainingLabel::Accepted, None, None).is_sft_eligible());
    }

    #[test]
    fn promotion_policy_default_tolerates_no_safety_failures() {
        let policy = PromotionPolicy::default();
        assert_eq!(policy.max_safety_failures, 0);
        assert!(policy.require_compatible);
        assert!(policy.min_quality_score > 0.0);
    }

    #[test]
    fn promotion_decision_maps_to_status() {
        assert!(PromotionDecision::Promote.is_promote());
        assert_eq!(
            PromotionDecision::Promote.resulting_status(),
            AdapterStatus::Active
        );
        let rejected = PromotionDecision::Reject {
            reasons: vec!["too risky".to_owned()],
        };
        assert!(!rejected.is_promote());
        assert_eq!(rejected.resulting_status(), AdapterStatus::FailedEval);
    }

    #[test]
    fn training_example_round_trips_through_json_compactly() {
        let ex = an_example(
            TrainingLabel::AcceptedWithMinorEdits,
            Some("fixed"),
            Some("ai"),
        );
        let json = serde_json::to_string(&ex).unwrap();
        let back: TrainingExample = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ex);
        // Empty collections are skipped.
        assert!(!json.contains("safety_flags"));
    }

    #[test]
    fn eval_metrics_default_is_empty_and_round_trips() {
        let metrics = EvalMetrics {
            accuracy: Some(0.9),
            win_rate: None,
            safety_failures: 0,
            quality_score: 0.85,
            extra: BTreeMap::new(),
        };
        let json = serde_json::to_string(&metrics).unwrap();
        assert!(!json.contains("win_rate"), "None is skipped");
        assert!(!json.contains("extra"), "empty map is skipped");
        let back: EvalMetrics = serde_json::from_str(&json).unwrap();
        assert_eq!(back, metrics);
    }
}
