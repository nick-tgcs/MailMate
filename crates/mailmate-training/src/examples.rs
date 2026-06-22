//! Deriving provider-neutral [`TrainingExample`]s from the per-task feedback rows — the
//! "derive on export" rule in code.
//!
//! There is no `training_examples` table: each feedback row maps deterministically to one
//! example here, so an export is a pure function of the captured feedback (and reproducible,
//! because each example's id is a stable hash of its source row id). Only the three feedback
//! kinds that are live today (classification, filing, rule-proposal) have derivers; the
//! body-carrying kinds (`draft`, `summary`, `task_extraction`) land with their AI functions
//! in later phases and slot in here with no change to the pipeline above.
//!
//! These derivers emit **metadata-level** examples (labels, folders, reason codes — no
//! readable body), so they are safe at any privacy ceiling. Redaction (for the future
//! body-carrying kinds) is enforced separately in [`crate::privacy`].

use std::collections::BTreeMap;

use mailmate_common::evidence::EvidenceSourceKind;
use mailmate_common::features::{FeatureValue, FeatureVector};
use mailmate_common::feedback::{
    ClassificationFeedbackRow, FeedbackPolarity, FilingFeedbackRow, ProposalOutcome,
    RuleProposalFeedbackRow,
};
use mailmate_common::hashing::stable_hash_hex;
use mailmate_common::training::{
    CandidateOutput, ContextFeatures, ExportPrivacyLevel, SourceFeedbackRef, TrainingExample,
    TrainingInput, TrainingLabel, TrainingTask,
};

/// The reproducible export id for an example derived from `source_id`.
#[must_use]
pub fn example_id(source_id: &str) -> String {
    format!("trn_{}", stable_hash_hex(&[source_id]))
}

/// The quality score a label implies (higher = stronger positive signal).
#[must_use]
fn quality_for(label: TrainingLabel) -> f64 {
    match label {
        TrainingLabel::Accepted => 1.0,
        TrainingLabel::AcceptedWithMinorEdits => 0.75,
        TrainingLabel::Corrected => 0.5,
        TrainingLabel::Undone => 0.1,
        TrainingLabel::Rejected
        | TrainingLabel::Discarded
        | TrainingLabel::BlockedByPolicy
        | TrainingLabel::Unsafe
        | TrainingLabel::Counterexample => 0.0,
    }
}

/// The label a message-task feedback row's polarity implies: a matching AI prediction is an
/// acceptance, a human override is a correction.
#[must_use]
fn label_for_polarity(polarity: FeedbackPolarity) -> TrainingLabel {
    match polarity {
        FeedbackPolarity::Positive => TrainingLabel::Accepted,
        FeedbackPolarity::Negative => TrainingLabel::Corrected,
    }
}

/// Render the deterministic, non-body salient features into a stable string map. A `Json`
/// escape-hatch feature is reduced to a placeholder so no structured payload leaks into the
/// example.
#[must_use]
fn features_to_context(features: &FeatureVector) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, value) in &features.features {
        // `account_id` is captured into salient features purely as a SCOPING dimension for
        // per-account rule induction — it is not a content signal and must not become a model input
        // (induction likewise never makes it a predicate). Exporting it would leak the user's
        // account identity into the training corpus, so it is dropped here.
        if name == "account_id" {
            continue;
        }
        let rendered = match value {
            FeatureValue::Bool(b) => b.to_string(),
            FeatureValue::Number(n) => n.to_string(),
            FeatureValue::Text(t) => t.clone(),
            FeatureValue::Json(_) => "<json>".to_owned(),
        };
        out.insert(name.clone(), rendered);
    }
    out
}

fn reason_context(
    reason_code: &Option<String>,
    extra: BTreeMap<String, String>,
) -> ContextFeatures {
    let mut extra = extra;
    if let Some(code) = reason_code {
        extra.insert("human_reason_code".to_owned(), code.clone());
    }
    ContextFeatures {
        sender_domain: None,
        thread_summary: None,
        forbidden_commitments: Vec::new(),
        extra,
    }
}

/// Derive the classification training example for one `classification_feedback` row.
#[must_use]
pub fn derive_classification(row: &ClassificationFeedbackRow) -> TrainingExample {
    let label = label_for_polarity(row.polarity);
    TrainingExample {
        id: example_id(row.id.as_str()),
        task: TrainingTask::Classification,
        source_feedback: SourceFeedbackRef {
            kind: EvidenceSourceKind::Classification,
            id: row.id.clone(),
        },
        privacy_level: ExportPrivacyLevel::Metadata,
        base_model_family: None,
        input: TrainingInput {
            system: None,
            instruction: "Classify this message.".to_owned(),
            context_features: reason_context(
                &row.human_reason_code,
                features_to_context(&row.salient_features),
            ),
        },
        candidate_output: row.ai_label.clone().map(CandidateOutput::new),
        user_corrected_output: Some(CandidateOutput::new(row.human_label.clone())),
        label,
        polarity: row.polarity,
        quality_score: quality_for(label),
        safety_flags: Vec::new(),
        created_at: row.created_at,
    }
}

/// Derive the filing training example for one `filing_feedback` row.
#[must_use]
pub fn derive_filing(row: &FilingFeedbackRow) -> TrainingExample {
    let label = label_for_polarity(row.polarity);
    let mut context = reason_context(&None, BTreeMap::new());
    context.sender_domain = row.sender_domain.clone();
    if let Some(basis) = &row.basis {
        context.extra.insert("basis".to_owned(), basis.clone());
    }
    TrainingExample {
        id: example_id(row.id.as_str()),
        task: TrainingTask::Filing,
        source_feedback: SourceFeedbackRef {
            kind: EvidenceSourceKind::Filing,
            id: row.id.clone(),
        },
        privacy_level: ExportPrivacyLevel::Metadata,
        base_model_family: None,
        input: TrainingInput {
            system: None,
            instruction: "Suggest a folder for this message.".to_owned(),
            context_features: context,
        },
        candidate_output: row
            .ai_suggested_folder
            .as_ref()
            .map(|f| CandidateOutput::new(f.as_str())),
        user_corrected_output: Some(CandidateOutput::new(row.human_chosen_folder.as_str())),
        label,
        polarity: row.polarity,
        quality_score: quality_for(label),
        safety_flags: Vec::new(),
        created_at: row.created_at,
    }
}

/// The label a proposal outcome implies for training.
#[must_use]
fn label_for_outcome(outcome: ProposalOutcome) -> TrainingLabel {
    match outcome {
        ProposalOutcome::Accepted => TrainingLabel::Accepted,
        ProposalOutcome::AcceptedWithEdits => TrainingLabel::AcceptedWithMinorEdits,
        ProposalOutcome::Rejected => TrainingLabel::Rejected,
        ProposalOutcome::DisabledLater => TrainingLabel::Undone,
    }
}

/// Derive the rule-proposal training example for one `rule_proposal_feedback` row. This
/// carries no body — it is a metadata signal about curator wording/granularity — so it has
/// no candidate/target output and is not SFT-eligible; it feeds the preference and
/// evaluation views.
#[must_use]
pub fn derive_rule_proposal(row: &RuleProposalFeedbackRow) -> TrainingExample {
    let label = label_for_outcome(row.outcome);
    TrainingExample {
        id: example_id(row.id.as_str()),
        task: TrainingTask::RuleProposal,
        source_feedback: SourceFeedbackRef {
            kind: EvidenceSourceKind::RuleProposal,
            id: row.id.clone(),
        },
        privacy_level: ExportPrivacyLevel::Metadata,
        base_model_family: None,
        input: TrainingInput {
            system: None,
            instruction: "Review this rule proposal.".to_owned(),
            context_features: reason_context(&row.human_reason_code, BTreeMap::new()),
        },
        candidate_output: None,
        user_corrected_output: None,
        label,
        polarity: row.polarity,
        quality_score: quality_for(label),
        safety_flags: Vec::new(),
        created_at: row.created_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::feedback::PinnedVersions;
    use mailmate_common::ids::{FeedbackId, FolderId, MessageId, ProposalId};
    use mailmate_common::time::Timestamp;

    fn classification_row(polarity: FeedbackPolarity) -> ClassificationFeedbackRow {
        let mut features = FeatureVector::new();
        features.insert("spf_pass", FeatureValue::Bool(true));
        features.insert("sender_history", FeatureValue::Number(12.0));
        features.insert("list_id", FeatureValue::Text("news.example".to_owned()));
        features.insert("blob", FeatureValue::Json(serde_json::json!({"k": "v"})));
        // A scoping-only feature folded in by the per-account correction capture — must NOT export.
        features.insert("account_id", FeatureValue::Text("work-account".to_owned()));
        ClassificationFeedbackRow {
            id: FeedbackId::from("clsfb_1"),
            message_id: MessageId::from("msg_1"),
            pinned_versions: PinnedVersions::default(),
            ai_label: Some("not_junk".to_owned()),
            ai_score: Some(0.2),
            ai_rationale: None,
            human_label: "phishing".to_owned(),
            human_reason_code: Some("spoofed_sender".to_owned()),
            human_reason_text: None,
            salient_features: features,
            polarity,
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn classification_derivation_is_reproducible_and_metadata_level() {
        let row = classification_row(FeedbackPolarity::Negative);
        let ex = derive_classification(&row);
        assert_eq!(ex.id, derive_classification(&row).id, "stable id");
        assert_eq!(ex.id, example_id("clsfb_1"));
        assert_eq!(ex.task, TrainingTask::Classification);
        assert_eq!(ex.privacy_level, ExportPrivacyLevel::Metadata);
        assert_eq!(
            ex.label,
            TrainingLabel::Corrected,
            "an override is a correction"
        );
        assert_eq!(ex.target_output().unwrap().body, "phishing");
        assert_eq!(ex.candidate_output.unwrap().body, "not_junk");
        // Salient features pass through, with the Json escape hatch reduced to a placeholder.
        let ctx = &ex.input.context_features.extra;
        assert_eq!(ctx.get("spf_pass").unwrap(), "true");
        assert_eq!(ctx.get("list_id").unwrap(), "news.example");
        assert_eq!(ctx.get("blob").unwrap(), "<json>");
        assert_eq!(ctx.get("human_reason_code").unwrap(), "spoofed_sender");
        // The account scoping dimension is NOT exported into the training corpus.
        assert!(
            ctx.get("account_id").is_none(),
            "account_id is scoping-only and must not leak into a model input"
        );
    }

    #[test]
    fn positive_classification_is_an_acceptance_with_full_quality() {
        let ex = derive_classification(&classification_row(FeedbackPolarity::Positive));
        assert_eq!(ex.label, TrainingLabel::Accepted);
        assert!((ex.quality_score - 1.0).abs() < f64::EPSILON);
        assert!(ex.is_sft_eligible());
    }

    #[test]
    fn filing_derivation_carries_domain_and_folder_target() {
        let row = FilingFeedbackRow {
            id: FeedbackId::from("filfb_1"),
            message_id: MessageId::from("msg_2"),
            pinned_versions: PinnedVersions::default(),
            sender_domain: Some("stripe.com".to_owned()),
            ai_suggested_folder: Some(FolderId::from("folder_inbox")),
            human_chosen_folder: FolderId::from("folder_receipts"),
            basis: Some("domain".to_owned()),
            matched_rule_id: None,
            polarity: FeedbackPolarity::Negative,
            created_at: Timestamp::now(),
        };
        let ex = derive_filing(&row);
        assert_eq!(ex.task, TrainingTask::Filing);
        assert_eq!(
            ex.input.context_features.sender_domain.as_deref(),
            Some("stripe.com")
        );
        assert_eq!(
            ex.input.context_features.extra.get("basis").unwrap(),
            "domain"
        );
        assert_eq!(ex.candidate_output.as_ref().unwrap().body, "folder_inbox");
        assert_eq!(ex.target_output().unwrap().body, "folder_receipts");
        assert_eq!(ex.label, TrainingLabel::Corrected);
    }

    #[test]
    fn rule_proposal_derivation_is_metadata_only_and_maps_outcomes() {
        let base = RuleProposalFeedbackRow {
            id: FeedbackId::from("rpffb_1"),
            proposal_id: ProposalId::from("prop_1"),
            pinned_versions: PinnedVersions::default(),
            outcome: ProposalOutcome::Accepted,
            human_reason_code: None,
            human_reason_text: None,
            polarity: FeedbackPolarity::Positive,
            created_at: Timestamp::now(),
        };
        let ex = derive_rule_proposal(&base);
        assert_eq!(ex.task, TrainingTask::RuleProposal);
        assert_eq!(ex.label, TrainingLabel::Accepted);
        assert!(ex.candidate_output.is_none() && ex.user_corrected_output.is_none());
        assert!(!ex.is_sft_eligible(), "no target -> not SFT-eligible");

        for (outcome, expected) in [
            (
                ProposalOutcome::AcceptedWithEdits,
                TrainingLabel::AcceptedWithMinorEdits,
            ),
            (ProposalOutcome::Rejected, TrainingLabel::Rejected),
            (ProposalOutcome::DisabledLater, TrainingLabel::Undone),
        ] {
            let row = RuleProposalFeedbackRow {
                outcome,
                polarity: outcome.polarity(),
                ..base.clone()
            };
            assert_eq!(derive_rule_proposal(&row).label, expected);
        }
    }
}
