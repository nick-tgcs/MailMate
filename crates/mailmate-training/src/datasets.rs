//! Assembling a [`TrainingDatasetRecord`] from derived examples: deterministic
//! train/validation/test/holdout splitting, per-view selection, counts, and the stable
//! `example_ids_hash` that is the dataset's content identity.
//!
//! The split is a pure function of each example's **source feedback id** (not a stored
//! column and not random), so the same captured feedback always partitions identically —
//! which is what lets an evaluation dataset stay a frozen hold-out across runs. Every
//! example is passed through [`crate::privacy::enforce_privacy`] before it is counted or
//! emitted, so a dataset can never carry content above its declared ceiling.

use mailmate_common::hashing::stable_bucket;
use mailmate_common::ids::DatasetId;
use mailmate_common::time::Timestamp;
use mailmate_common::training::{
    DatasetSplit, DatasetType, ExportFormat, ExportPrivacyLevel, TrainingDatasetRecord,
    TrainingExample,
};

use crate::privacy::enforce_privacy;

/// The deterministic 70 / 15 / 10 / 5 partition for a source id.
#[must_use]
pub fn assign_split(source_id: &str) -> DatasetSplit {
    match stable_bucket(source_id, 100) {
        0..=69 => DatasetSplit::Train,
        70..=84 => DatasetSplit::Validation,
        85..=94 => DatasetSplit::Test,
        _ => DatasetSplit::Holdout,
    }
}

/// Whether an example forms a preference pair: a candidate and a differing human correction.
#[must_use]
pub fn is_preference_eligible(example: &TrainingExample) -> bool {
    match (&example.candidate_output, &example.user_corrected_output) {
        (Some(candidate), Some(corrected)) => candidate.body != corrected.body,
        _ => false,
    }
}

/// Whether an example belongs in `dataset_type`'s view, given its split. The training views
/// (SFT, preference) draw only from train/validation; evaluation is the frozen test/holdout
/// hold-out; safety counterexamples are drawn from any split.
#[must_use]
fn select_for(dataset_type: DatasetType, example: &TrainingExample, split: DatasetSplit) -> bool {
    let in_training = matches!(split, DatasetSplit::Train | DatasetSplit::Validation);
    let in_holdout = matches!(split, DatasetSplit::Test | DatasetSplit::Holdout);
    match dataset_type {
        DatasetType::Sft => in_training && example.is_sft_eligible(),
        DatasetType::Preference => in_training && is_preference_eligible(example),
        DatasetType::Evaluation => in_holdout,
        DatasetType::SafetyCounterexample => {
            example.label.is_safety_negative() || !example.safety_flags.is_empty()
        }
    }
}

/// A built dataset: the durable record plus the (privacy-enforced, view-selected) examples
/// the export will render. The examples are not persisted — only `record` is.
#[derive(Clone, Debug, PartialEq)]
pub struct DatasetPlan {
    /// The durable dataset record.
    pub record: TrainingDatasetRecord,
    /// The selected, privacy-enforced examples (rendered by [`crate::export`]).
    pub examples: Vec<TrainingExample>,
}

impl DatasetPlan {
    /// Whether the plan selected no examples.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.examples.is_empty()
    }
}

/// Build the `dataset_type` view from `examples`, enforcing `ceiling` on every example.
///
/// `now` and the fresh dataset id are injected by the caller (the pipeline, via its clock)
/// so the rest of the function is a pure, reproducible transform.
#[must_use]
pub fn build_dataset(
    name: impl Into<String>,
    dataset_type: DatasetType,
    base_model_family: Option<String>,
    ceiling: ExportPrivacyLevel,
    now: Timestamp,
    examples: impl IntoIterator<Item = TrainingExample>,
) -> DatasetPlan {
    let mut selected = Vec::new();
    let mut positive_count = 0usize;
    let mut negative_count = 0usize;
    let mut validation_count = 0usize;
    let mut test_count = 0usize;
    let mut privacy_level = ExportPrivacyLevel::Metadata;
    let mut source_ids = Vec::new();

    for example in examples {
        let split = assign_split(example.source_feedback.id.as_str());
        let example = enforce_privacy(example, ceiling);
        if !select_for(dataset_type, &example, split) {
            continue;
        }
        if example.label.is_positive() {
            positive_count += 1;
        } else {
            negative_count += 1;
        }
        match split {
            DatasetSplit::Validation => validation_count += 1,
            DatasetSplit::Test => test_count += 1,
            _ => {}
        }
        privacy_level = privacy_level.max(example.privacy_level);
        source_ids.push(example.source_feedback.id.as_str().to_owned());
        selected.push(example);
    }

    source_ids.sort();
    let id_refs: Vec<&str> = source_ids.iter().map(String::as_str).collect();
    let example_ids_hash = mailmate_common::hashing::stable_hash_hex(&id_refs);

    let record = TrainingDatasetRecord {
        id: DatasetId::fresh(),
        name: name.into(),
        dataset_type,
        base_model_family,
        example_ids_hash,
        positive_count,
        negative_count,
        validation_count,
        test_count,
        privacy_level,
        export_format: ExportFormat::default_for(dataset_type),
        artifact_path: None,
        created_at: now,
    };
    DatasetPlan {
        record,
        examples: selected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::evidence::EvidenceSourceKind;
    use mailmate_common::ids::FeedbackId;
    use mailmate_common::training::{
        CandidateOutput, ContextFeatures, SourceFeedbackRef, TrainingInput, TrainingLabel,
        TrainingTask,
    };

    fn example(
        source_id: &str,
        label: TrainingLabel,
        candidate: Option<&str>,
        corrected: Option<&str>,
        flags: Vec<mailmate_common::training::SafetyFlag>,
    ) -> TrainingExample {
        TrainingExample {
            id: format!("trn_{source_id}"),
            task: TrainingTask::DraftReply,
            source_feedback: SourceFeedbackRef {
                kind: EvidenceSourceKind::Draft,
                id: FeedbackId::from(source_id),
            },
            privacy_level: ExportPrivacyLevel::Metadata,
            base_model_family: None,
            input: TrainingInput {
                system: None,
                instruction: "x".to_owned(),
                context_features: ContextFeatures::default(),
            },
            candidate_output: candidate.map(CandidateOutput::new),
            user_corrected_output: corrected.map(CandidateOutput::new),
            label,
            polarity: label.polarity(),
            quality_score: 0.5,
            safety_flags: flags,
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn split_assignment_is_deterministic_and_covers_every_bucket() {
        // The same id always lands in the same split.
        assert_eq!(assign_split("fb_1"), assign_split("fb_1"));
        // Across many ids every split is reachable (the partition is non-degenerate).
        let mut seen = std::collections::HashSet::new();
        for i in 0..500 {
            seen.insert(assign_split(&format!("fb_{i}")));
        }
        assert!(seen.contains(&DatasetSplit::Train));
        assert!(seen.contains(&DatasetSplit::Validation));
        assert!(seen.contains(&DatasetSplit::Test));
        assert!(seen.contains(&DatasetSplit::Holdout));
    }

    #[test]
    fn sft_view_selects_positive_training_examples_with_a_target() {
        // Build enough examples that some land in train; pick ids known to be train.
        let train_ids: Vec<String> = (0..200)
            .map(|i| format!("s{i}"))
            .filter(|id| assign_split(id) == DatasetSplit::Train)
            .take(3)
            .collect();
        let examples: Vec<_> = train_ids
            .iter()
            .map(|id| example(id, TrainingLabel::Accepted, Some("ai"), None, vec![]))
            .collect();
        let plan = build_dataset(
            "d",
            DatasetType::Sft,
            None,
            ExportPrivacyLevel::Metadata,
            Timestamp::now(),
            examples,
        );
        assert_eq!(plan.examples.len(), 3);
        assert_eq!(plan.record.positive_count, 3);
        assert_eq!(plan.record.dataset_type, DatasetType::Sft);
        assert_eq!(plan.record.export_format, ExportFormat::JsonlChat);
        assert!(plan.record.id.as_str().starts_with("ds_"));
        // Hash is stable for the same id set, order-independent.
        let plan2 = build_dataset(
            "d",
            DatasetType::Sft,
            None,
            ExportPrivacyLevel::Metadata,
            Timestamp::now(),
            train_ids
                .iter()
                .rev()
                .map(|id| example(id, TrainingLabel::Accepted, Some("ai"), None, vec![])),
        );
        assert_eq!(plan.record.example_ids_hash, plan2.record.example_ids_hash);
    }

    #[test]
    fn evaluation_view_takes_the_holdout_not_the_training_rows() {
        let test_id = (0..500)
            .map(|i| format!("e{i}"))
            .find(|id| assign_split(id) == DatasetSplit::Test)
            .unwrap();
        let train_id = (0..500)
            .map(|i| format!("t{i}"))
            .find(|id| assign_split(id) == DatasetSplit::Train)
            .unwrap();
        let plan = build_dataset(
            "eval",
            DatasetType::Evaluation,
            None,
            ExportPrivacyLevel::Metadata,
            Timestamp::now(),
            vec![
                example(&test_id, TrainingLabel::Accepted, Some("ai"), None, vec![]),
                example(&train_id, TrainingLabel::Accepted, Some("ai"), None, vec![]),
            ],
        );
        // Only the test-split row is in the evaluation view.
        assert_eq!(plan.examples.len(), 1);
        assert_eq!(plan.examples[0].source_feedback.id.as_str(), test_id);
        assert_eq!(plan.record.test_count, 1);
    }

    #[test]
    fn preference_view_requires_a_differing_pair() {
        let train_id = (0..500)
            .map(|i| format!("p{i}"))
            .find(|id| assign_split(id) == DatasetSplit::Train)
            .unwrap();
        // candidate != corrected -> eligible
        let plan = build_dataset(
            "pref",
            DatasetType::Preference,
            None,
            ExportPrivacyLevel::Metadata,
            Timestamp::now(),
            vec![example(
                &train_id,
                TrainingLabel::Corrected,
                Some("bad"),
                Some("good"),
                vec![],
            )],
        );
        assert_eq!(plan.examples.len(), 1);
        assert_eq!(plan.record.export_format, ExportFormat::PreferenceJsonl);
        // identical candidate/corrected -> not a pair
        let plan = build_dataset(
            "pref",
            DatasetType::Preference,
            None,
            ExportPrivacyLevel::Metadata,
            Timestamp::now(),
            vec![example(
                &train_id,
                TrainingLabel::Accepted,
                Some("same"),
                Some("same"),
                vec![],
            )],
        );
        assert!(plan.is_empty());
    }

    #[test]
    fn safety_view_collects_flagged_or_safety_negative_examples() {
        let plan = build_dataset(
            "safe",
            DatasetType::SafetyCounterexample,
            None,
            ExportPrivacyLevel::Metadata,
            Timestamp::now(),
            vec![
                example("a", TrainingLabel::Unsafe, None, None, vec![]),
                example(
                    "b",
                    TrainingLabel::Accepted,
                    Some("ok"),
                    None,
                    vec![mailmate_common::training::SafetyFlag::PaymentChange],
                ),
                example("c", TrainingLabel::Accepted, Some("ok"), None, vec![]),
            ],
        );
        // 'a' (unsafe label) and 'b' (flagged) are in; 'c' is not.
        assert_eq!(plan.examples.len(), 2);
    }

    #[test]
    fn privacy_ceiling_is_applied_and_recorded_as_the_max() {
        let train_id = (0..500)
            .map(|i| format!("v{i}"))
            .find(|id| assign_split(id) == DatasetSplit::Train)
            .unwrap();
        let mut ex = example(
            &train_id,
            TrainingLabel::Accepted,
            Some("mail a@b.com now"),
            None,
            vec![],
        );
        ex.privacy_level = ExportPrivacyLevel::Full;
        let plan = build_dataset(
            "d",
            DatasetType::Sft,
            None,
            ExportPrivacyLevel::Redacted,
            Timestamp::now(),
            vec![ex],
        );
        assert_eq!(plan.record.privacy_level, ExportPrivacyLevel::Redacted);
        assert!(plan.examples[0]
            .candidate_output
            .as_ref()
            .unwrap()
            .body
            .contains("[email]"));
    }
}
