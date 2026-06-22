//! The on-device Tier-2 training service (Phase 8): the composition-root glue that turns the
//! user's accumulated classification corrections into a freshly-trained, gated, **loadable**
//! Burn artifact and hot-swaps it into the live cascade.
//!
//! It reads `classification_feedback` (the source of truth), maps each corrected label onto the
//! Tier-2 spam/ham axis, runs [`mailmate_ml::train_tier2_eval_gate`] (train → save → reload →
//! held-out eval → precision gate → atomic promote), and — only when the gate passes — loads
//! the activated artifact and swaps it into the cascade's [`SwappableTier2`] so it serves the
//! next classification without a host restart. Determinism-first: the model is the teacher; it
//! only becomes the predictor once its held-out precision is vetted.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mailmate_common::error::StorageError;
use mailmate_common::features::LabeledExample;
use mailmate_common::feedback::{ClassificationFeedback, ClassificationFeedbackQuery};
use mailmate_ml::tier2_burn::CALIBRATION_VERSION;
use mailmate_ml::{
    train_tier2_eval_gate, BurnTier2Classifier, SwappableTier2, Tier2EvalMetrics, Tier2ModelError,
    Tier2TrainConfig, Tier2TrainingReport, DEFAULT_PRECISION_GATE,
};
use mailmate_ports::storage::FeedbackRepository;
use mailmate_ports::tier2_classifier::Tier2Classifier;

/// At most this many corrections feed one training run (newest first) — a sane ceiling so a
/// long-lived install does not load an unbounded corpus into memory.
const MAX_ROWS: usize = 20_000;

/// Corrected labels that map onto the Tier-2 *positive* (junk/spam) class; every other label is
/// a "this is not junk" signal and maps to the negative class. Lowercased before lookup.
const JUNKISH: &[&str] = &["spam", "junk", "phishing", "suspicious", "malware"];

/// The calibration tag a swapped-in Burn Tier-2 model stamps — surfaced so a caller can confirm
/// the cascade is serving the trained artifact.
pub const TIER2_CALIBRATION_VERSION: &str = CALIBRATION_VERSION;

/// Errors from a training run.
#[derive(Debug)]
pub enum Tier2TrainingError {
    /// Reading the feedback corpus failed.
    Storage(StorageError),
    /// Training, saving, reloading, or evaluating the model failed.
    Model(Tier2ModelError),
}

impl std::fmt::Display for Tier2TrainingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(e) => write!(f, "reading the correction corpus failed: {e}"),
            Self::Model(e) => write!(f, "training the tier-2 model failed: {e}"),
        }
    }
}

impl std::error::Error for Tier2TrainingError {}

impl From<StorageError> for Tier2TrainingError {
    fn from(e: StorageError) -> Self {
        Self::Storage(e)
    }
}
impl From<Tier2ModelError> for Tier2TrainingError {
    fn from(e: Tier2ModelError) -> Self {
        Self::Model(e)
    }
}

/// Map a corrected human label onto the Tier-2 binary class.
fn to_binary(human_label: &str, positive: &str, negative: &str) -> String {
    if JUNKISH.contains(&human_label.to_ascii_lowercase().as_str()) {
        positive.to_owned()
    } else {
        negative.to_owned()
    }
}

/// The Tier-2 training service: trains from corrections and hot-swaps the cascade's Tier-2.
#[derive(Clone)]
pub struct Tier2TrainingService {
    swappable: Arc<SwappableTier2>,
    feedback: Arc<dyn FeedbackRepository<ClassificationFeedback>>,
    candidate_dir: PathBuf,
    active_dir: PathBuf,
    positive_label: String,
    negative_label: String,
    max_rows: usize,
    precision_gate: f64,
}

impl Tier2TrainingService {
    /// Wire the service with the default tuning (corpus ceiling `MAX_ROWS`, precision gate
    /// `DEFAULT_PRECISION_GATE`).
    #[must_use]
    pub fn new(
        swappable: Arc<SwappableTier2>,
        feedback: Arc<dyn FeedbackRepository<ClassificationFeedback>>,
        candidate_dir: PathBuf,
        active_dir: PathBuf,
        positive_label: impl Into<String>,
        negative_label: impl Into<String>,
    ) -> Self {
        Self {
            swappable,
            feedback,
            candidate_dir,
            active_dir,
            positive_label: positive_label.into(),
            negative_label: negative_label.into(),
            max_rows: MAX_ROWS,
            precision_gate: DEFAULT_PRECISION_GATE,
        }
    }

    /// Override the training tuning from config (`[tier2]`): the corpus ceiling fed to one run
    /// and the held-out precision an artifact must clear before it activates. A zero or
    /// out-of-range value keeps the default (the config validator rejects bad values up front, so
    /// this is just a belt-and-braces guard).
    #[must_use]
    pub fn with_tuning(mut self, max_rows: usize, precision_gate: f64) -> Self {
        if max_rows > 0 {
            self.max_rows = max_rows;
        }
        if precision_gate > 0.0 && precision_gate <= 1.0 {
            self.precision_gate = precision_gate;
        }
        self
    }

    /// The active artifact directory (a previously-promoted model lives here).
    #[must_use]
    pub fn active_dir(&self) -> &Path {
        &self.active_dir
    }

    /// Try to load a previously-activated artifact as a Tier-2 model — the cascade's initial
    /// backing at startup, when one was promoted in an earlier session. `None` (cold start, fall
    /// back to the online model) on a missing or unreadable artifact.
    #[must_use]
    pub fn load_active(active_dir: &Path) -> Option<Arc<dyn Tier2Classifier>> {
        BurnTier2Classifier::load(active_dir)
            .ok()
            .map(|c| Arc::new(c) as Arc<dyn Tier2Classifier>)
    }

    /// Run a full training pass: read corrections → train+gate → swap on activation.
    ///
    /// # Errors
    /// [`Tier2TrainingError`] if reading the corpus or the training/eval/save fails.
    pub async fn run(&self) -> Result<Tier2TrainingReport, Tier2TrainingError> {
        let rows = self
            .feedback
            .query(ClassificationFeedbackQuery {
                limit: Some(self.max_rows),
                ..Default::default()
            })
            .await?;
        let examples: Vec<LabeledExample> = rows
            .iter()
            .map(|r| LabeledExample {
                features: r.salient_features.clone(),
                label: to_binary(&r.human_label, &self.positive_label, &self.negative_label),
            })
            .collect();

        if examples.is_empty() {
            // Nothing to learn from yet — report honestly, never fabricate a model.
            return Ok(Tier2TrainingReport {
                train_count: 0,
                eval: Tier2EvalMetrics {
                    n: 0,
                    accuracy: 0.0,
                    precision: 0.0,
                    recall: 0.0,
                    true_positives: 0,
                    false_positives: 0,
                },
                activated: false,
                precision_gate: self.precision_gate,
                artifact_path: None,
            });
        }

        let report = train_tier2_eval_gate(
            &examples,
            &self.positive_label,
            &self.negative_label,
            &Tier2TrainConfig::default(),
            &self.candidate_dir,
            &self.active_dir,
            self.precision_gate,
        )?;

        if report.activated {
            // Load the just-promoted artifact and swap it into the cascade in place. A reload
            // failure here is non-fatal: the model stays on disk and is picked up next restart.
            match BurnTier2Classifier::load(&self.active_dir) {
                Ok(clf) => self.swappable.swap(Arc::new(clf)),
                Err(e) => {
                    log::warn!("activated tier-2 artifact failed to reload for hot-swap: {e}")
                }
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junkish_labels_map_to_the_positive_class_case_insensitively() {
        assert_eq!(to_binary("Spam", "spam", "ham"), "spam");
        assert_eq!(to_binary("PHISHING", "spam", "ham"), "spam");
        assert_eq!(to_binary("malware", "spam", "ham"), "spam");
    }

    #[test]
    fn non_junkish_labels_map_to_the_negative_class() {
        assert_eq!(to_binary("newsletter", "spam", "ham"), "ham");
        assert_eq!(to_binary("receipt", "spam", "ham"), "ham");
        assert_eq!(to_binary("", "spam", "ham"), "ham");
    }

    #[test]
    fn a_storage_error_converts_and_displays_as_a_corpus_read_failure() {
        let err: Tier2TrainingError = StorageError::Serialization("bad row".to_owned()).into();
        assert!(matches!(err, Tier2TrainingError::Storage(_)));
        assert!(err
            .to_string()
            .contains("reading the correction corpus failed"));
    }

    #[test]
    fn a_model_error_converts_and_displays_as_a_training_failure() {
        let err: Tier2TrainingError = Tier2ModelError::NoExamples.into();
        assert!(matches!(err, Tier2TrainingError::Model(_)));
        assert!(err.to_string().contains("training the tier-2 model failed"));
    }
}
