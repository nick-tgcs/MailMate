//! The per-task feedback vocabulary — the **training source of truth**.
//!
//! Each AI function owns one self-contained feedback table holding its own provenance
//! ([`PinnedVersions`]), the AI proposal, the human correction, and a prompted reason.
//! They are the single writer of their task's signal: a correction lands in exactly one
//! table, and evidence/outcomes are *derived* from these rows, never re-recorded.
//!
//! Phase 7 ships the two tables whose AI functions are already live — classification
//! (P1) and filing (P2). The remaining tables (`draft`, `summary`, `task_extraction`,
//! `rule_proposal`, `followup`) ship with their functions in later phases; the
//! [`EvidenceSourceKind`](crate::evidence::EvidenceSourceKind) vocabulary already names
//! them so nothing downstream has to change when they land.
//!
//! [`TaskFeedbackKind`] is the generic seam the storage `FeedbackRepository<F>` is
//! parameterized over: one mechanism, one table per kind.

use serde::{Deserialize, Serialize};

use crate::evidence::EvidenceSourceKind;
use crate::features::FeatureVector;
use crate::ids::{
    fresh_prefixed, FeedbackId, FolderId, MessageId, ProposalId, RuleId, RuleVersionId,
};
use crate::time::Timestamp;

/// Whether the AI was right (`Positive`) or wrong (`Negative`) on this row. A correction
/// prompts on **divergence**, so most captured rows are `Negative`; an accepted
/// suggestion captured for reinforcement is `Positive`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackPolarity {
    /// The AI prediction/suggestion matched what the human did.
    Positive,
    /// The human corrected/overrode the AI.
    Negative,
}

impl FeedbackPolarity {
    /// The stable snake_case label stored in the `polarity` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Positive => "positive",
            Self::Negative => "negative",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "positive" => Some(Self::Positive),
            "negative" => Some(Self::Negative),
            _ => None,
        }
    }
}

/// Copy-at-event provenance: the exact versions that produced the prediction this row
/// corrects. Serialized whole into the `pinned_versions_json` column.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct PinnedVersions {
    /// Rule versions that fired.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rule_version_ids: Vec<RuleVersionId>,
    /// The prompt-template version, when a provider produced the prediction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_template_version: Option<String>,
    /// The model version (Tier-2 calibration or a LoRA tag).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_version: Option<String>,
    /// The calibration version pinned at prediction time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration_version: Option<String>,
}

/// `classification_feedback` — junk / phishing / priority / labels.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ClassificationFeedbackRow {
    /// The row id (`clsfb_…`).
    pub id: FeedbackId,
    /// The message this judgement concerns.
    pub message_id: MessageId,
    /// Provenance of the corrected prediction.
    pub pinned_versions: PinnedVersions,
    /// The label the AI assigned (`junk`, `phishing`, …), if any.
    pub ai_label: Option<String>,
    /// The AI's confidence, if a model produced it.
    pub ai_score: Option<f64>,
    /// What the model keyed on, redacted.
    pub ai_rationale: Option<String>,
    /// The corrected (human) label.
    pub human_label: String,
    /// Why corrected — a task-specific chip code (`subscribed_list`, `known_sender`, …).
    pub human_reason_code: Option<String>,
    /// Freeform fallback reason.
    pub human_reason_text: Option<String>,
    /// The deterministic features that mattered (spf, sender history, list-id, domain).
    pub salient_features: FeatureVector,
    /// Whether the AI was right or wrong.
    pub polarity: FeedbackPolarity,
    /// When captured.
    pub created_at: Timestamp,
}

/// `filing_feedback` — folder suggestion / move.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FilingFeedbackRow {
    /// The row id (`filfb_…`).
    pub id: FeedbackId,
    /// The message that was filed.
    pub message_id: MessageId,
    /// Provenance of the suggestion.
    pub pinned_versions: PinnedVersions,
    /// The sender's domain, retained so domain-clustered proposals are deterministic
    /// without re-joining the message (the doc's "same domain → same folder" threshold).
    pub sender_domain: Option<String>,
    /// The folder the AI suggested, if any.
    pub ai_suggested_folder: Option<FolderId>,
    /// The folder the user actually chose.
    pub human_chosen_folder: FolderId,
    /// The basis the move keys on (`sender`, `domain`, `subject_keyword`, `thread`, …).
    pub basis: Option<String>,
    /// The rule that fired, if any.
    pub matched_rule_id: Option<RuleId>,
    /// Whether the suggestion matched the human choice.
    pub polarity: FeedbackPolarity,
    /// When captured.
    pub created_at: Timestamp,
}

/// What a human decided about a curator/learning proposal — the disposition recorded in
/// `rule_proposal_feedback` so the curator loop is itself learnable (a repeatedly-rejected
/// proposal shape becomes evidence not to re-propose it).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalOutcome {
    /// Accepted as-is.
    Accepted,
    /// Accepted after the human edited the draft.
    AcceptedWithEdits,
    /// Rejected outright.
    Rejected,
    /// Accepted earlier, then the derived rule was disabled.
    DisabledLater,
}

impl ProposalOutcome {
    /// The stable snake_case label stored in `rule_proposal_feedback.outcome`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::AcceptedWithEdits => "accepted_with_edits",
            Self::Rejected => "rejected",
            Self::DisabledLater => "disabled_later",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "accepted" => Some(Self::Accepted),
            "accepted_with_edits" => Some(Self::AcceptedWithEdits),
            "rejected" => Some(Self::Rejected),
            "disabled_later" => Some(Self::DisabledLater),
            _ => None,
        }
    }

    /// Whether the outcome accepted the proposal (with or without edits).
    #[must_use]
    pub fn is_acceptance(self) -> bool {
        matches!(self, Self::Accepted | Self::AcceptedWithEdits)
    }

    /// The polarity this outcome implies: an acceptance reinforces the proposer, a
    /// rejection corrects it.
    #[must_use]
    pub fn polarity(self) -> FeedbackPolarity {
        if self.is_acceptance() {
            FeedbackPolarity::Positive
        } else {
            FeedbackPolarity::Negative
        }
    }
}

/// `rule_proposal_feedback` — the outcome of a human reviewing an agent proposal. Unlike
/// the task-feedback tables this is **proposal-scoped, not message-scoped** (it links a
/// `proposal_id`, has no `message_id`), so it is the single writer of "what the human did
/// with this proposal" — distinct from the audit timeline's `proposal_reviewed` event.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RuleProposalFeedbackRow {
    /// The row id (`rpffb_…`).
    pub id: FeedbackId,
    /// The proposal this decision concerns.
    pub proposal_id: ProposalId,
    /// Provenance pinned at review time.
    pub pinned_versions: PinnedVersions,
    /// What the human decided.
    pub outcome: ProposalOutcome,
    /// Why — a chip code, if given.
    pub human_reason_code: Option<String>,
    /// Freeform fallback reason.
    pub human_reason_text: Option<String>,
    /// Whether this reinforces (`Positive`) or corrects (`Negative`) the proposer.
    pub polarity: FeedbackPolarity,
    /// When reviewed.
    pub created_at: Timestamp,
}

/// A captured correction, tagged by which table owns it. The learning engine routes each
/// variant to its single-owner [`FeedbackRepository`](crate placeholder) and derives
/// evidence from it — it is never re-recorded elsewhere.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "feedback_kind", rename_all = "snake_case")]
pub enum TaskFeedback {
    /// A `classification_feedback` row.
    Classification(ClassificationFeedbackRow),
    /// A `filing_feedback` row.
    Filing(FilingFeedbackRow),
}

impl TaskFeedback {
    /// The row's id.
    #[must_use]
    pub fn id(&self) -> &FeedbackId {
        match self {
            Self::Classification(row) => &row.id,
            Self::Filing(row) => &row.id,
        }
    }

    /// The message the correction concerns.
    #[must_use]
    pub fn message_id(&self) -> &MessageId {
        match self {
            Self::Classification(row) => &row.message_id,
            Self::Filing(row) => &row.message_id,
        }
    }

    /// Which feedback source this row belongs to.
    #[must_use]
    pub fn source_kind(&self) -> EvidenceSourceKind {
        match self {
            Self::Classification(_) => EvidenceSourceKind::Classification,
            Self::Filing(_) => EvidenceSourceKind::Filing,
        }
    }

    /// Whether the AI was right or wrong on this row.
    #[must_use]
    pub fn polarity(&self) -> FeedbackPolarity {
        match self {
            Self::Classification(row) => row.polarity,
            Self::Filing(row) => row.polarity,
        }
    }
}

/// The generic seam over the per-task feedback tables: one type per table, naming its
/// row type, its query type, its evidence source, and its DB id prefix. The storage
/// `FeedbackRepository<F>` is parameterized over this — one mechanism, many tables.
pub trait TaskFeedbackKind: Send + Sync + 'static {
    /// The persisted row type for this table.
    type Row: Send;
    /// The query type for this table.
    type Query: Send + Default;
    /// Which evidence source these rows are.
    const SOURCE_KIND: EvidenceSourceKind;
    /// The human-readable DB id prefix for a row (`clsfb`, `filfb`, …).
    const ID_PREFIX: &'static str;
}

/// The classification-feedback kind.
#[derive(Clone, Copy, Debug)]
pub struct ClassificationFeedback;

/// The filing-feedback kind.
#[derive(Clone, Copy, Debug)]
pub struct FilingFeedback;

/// Query over `classification_feedback`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ClassificationFeedbackQuery {
    /// Restrict to one message.
    pub message_id: Option<MessageId>,
    /// Restrict to one corrected label (`phishing`, `spam`, …).
    pub human_label: Option<String>,
    /// Cap the number of rows (newest first).
    pub limit: Option<usize>,
}

/// Query over `filing_feedback`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct FilingFeedbackQuery {
    /// Restrict to one message.
    pub message_id: Option<MessageId>,
    /// Restrict to one chosen folder.
    pub human_chosen_folder: Option<FolderId>,
    /// Cap the number of rows (newest first).
    pub limit: Option<usize>,
}

impl TaskFeedbackKind for ClassificationFeedback {
    type Row = ClassificationFeedbackRow;
    type Query = ClassificationFeedbackQuery;
    const SOURCE_KIND: EvidenceSourceKind = EvidenceSourceKind::Classification;
    const ID_PREFIX: &'static str = "clsfb";
}

impl ClassificationFeedback {
    /// Mint a fresh, table-prefixed id (`clsfb_…`).
    #[must_use]
    pub fn fresh_id() -> FeedbackId {
        FeedbackId::from(fresh_prefixed(Self::ID_PREFIX))
    }
}

impl TaskFeedbackKind for FilingFeedback {
    type Row = FilingFeedbackRow;
    type Query = FilingFeedbackQuery;
    const SOURCE_KIND: EvidenceSourceKind = EvidenceSourceKind::Filing;
    const ID_PREFIX: &'static str = "filfb";
}

impl FilingFeedback {
    /// Mint a fresh, table-prefixed id (`filfb_…`).
    #[must_use]
    pub fn fresh_id() -> FeedbackId {
        FeedbackId::from(fresh_prefixed(Self::ID_PREFIX))
    }
}

/// The rule-proposal-feedback kind. Lands with the agent curator (Phase 8); it plugs into
/// the same `FeedbackRepository<F>` generic as the message-scoped tables.
#[derive(Clone, Copy, Debug)]
pub struct RuleProposalFeedback;

/// Query over `rule_proposal_feedback`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct RuleProposalFeedbackQuery {
    /// Restrict to one proposal.
    pub proposal_id: Option<ProposalId>,
    /// Cap the number of rows (newest first).
    pub limit: Option<usize>,
}

impl TaskFeedbackKind for RuleProposalFeedback {
    type Row = RuleProposalFeedbackRow;
    type Query = RuleProposalFeedbackQuery;
    const SOURCE_KIND: EvidenceSourceKind = EvidenceSourceKind::RuleProposal;
    const ID_PREFIX: &'static str = "rpffb";
}

impl RuleProposalFeedback {
    /// Mint a fresh, table-prefixed id (`rpffb_…`).
    #[must_use]
    pub fn fresh_id() -> FeedbackId {
        FeedbackId::from(fresh_prefixed(Self::ID_PREFIX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classification_row() -> ClassificationFeedbackRow {
        ClassificationFeedbackRow {
            id: ClassificationFeedback::fresh_id(),
            message_id: MessageId::from("msg_1"),
            pinned_versions: PinnedVersions::default(),
            ai_label: Some("not_junk".to_owned()),
            ai_score: Some(0.2),
            ai_rationale: None,
            human_label: "phishing".to_owned(),
            human_reason_code: Some("spoofed_sender".to_owned()),
            human_reason_text: None,
            salient_features: FeatureVector::new(),
            polarity: FeedbackPolarity::Negative,
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn task_feedback_projects_its_owner_and_polarity() {
        let fb = TaskFeedback::Classification(classification_row());
        assert_eq!(fb.source_kind(), EvidenceSourceKind::Classification);
        assert_eq!(fb.polarity(), FeedbackPolarity::Negative);
        assert_eq!(fb.message_id(), &MessageId::from("msg_1"));
        assert!(fb.id().as_str().starts_with("clsfb_"));
    }

    #[test]
    fn kinds_carry_distinct_prefixes_and_sources() {
        assert_eq!(ClassificationFeedback::ID_PREFIX, "clsfb");
        assert_eq!(FilingFeedback::ID_PREFIX, "filfb");
        assert_eq!(
            ClassificationFeedback::SOURCE_KIND,
            EvidenceSourceKind::Classification
        );
        assert_eq!(FilingFeedback::SOURCE_KIND, EvidenceSourceKind::Filing);
        assert!(FilingFeedback::fresh_id().as_str().starts_with("filfb_"));
        assert_eq!(RuleProposalFeedback::ID_PREFIX, "rpffb");
        assert_eq!(
            RuleProposalFeedback::SOURCE_KIND,
            EvidenceSourceKind::RuleProposal
        );
        assert!(RuleProposalFeedback::fresh_id()
            .as_str()
            .starts_with("rpffb_"));
    }

    #[test]
    fn proposal_outcome_labels_and_polarity_are_stable() {
        for outcome in [
            ProposalOutcome::Accepted,
            ProposalOutcome::AcceptedWithEdits,
            ProposalOutcome::Rejected,
            ProposalOutcome::DisabledLater,
        ] {
            assert_eq!(
                ProposalOutcome::from_db_str(outcome.as_str()),
                Some(outcome)
            );
        }
        assert_eq!(ProposalOutcome::from_db_str("nope"), None);
        assert!(ProposalOutcome::Accepted.is_acceptance());
        assert!(ProposalOutcome::AcceptedWithEdits.is_acceptance());
        assert!(!ProposalOutcome::Rejected.is_acceptance());
        assert_eq!(
            ProposalOutcome::Accepted.polarity(),
            FeedbackPolarity::Positive
        );
        assert_eq!(
            ProposalOutcome::Rejected.polarity(),
            FeedbackPolarity::Negative
        );
    }

    #[test]
    fn pinned_versions_skips_empty_fields_in_json() {
        let json = serde_json::to_string(&PinnedVersions::default()).unwrap();
        assert_eq!(json, "{}", "an empty provenance serializes compactly");
        let pinned = PinnedVersions {
            calibration_version: Some("logreg-identity-v1".to_owned()),
            ..PinnedVersions::default()
        };
        let json = serde_json::to_string(&pinned).unwrap();
        assert!(json.contains("logreg-identity-v1"));
        assert!(!json.contains("rule_version_ids"), "empty vec is skipped");
    }
}
