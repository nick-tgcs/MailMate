//! An online binary logistic-regression [`Tier2Classifier`] — the lightweight, always-on
//! alternative to the Burn discriminative classifier.
//!
//! It featurizes a [`FeatureVector`] deterministically (bools → 0/1, numbers as-is, text →
//! one-hot `name=value` keys), maintains a sparse weight map, predicts a calibrated
//! probability via the logistic function, and learns online from each labelled example by
//! one SGD step on the logistic loss with **L2 weight-decay**. Pure Rust, no model runtime —
//! same input → same output, fully testable without a GPU or Burn.
//!
//! Two guards keep a sparse online model honest:
//! - a **per-feature confidence floor** — a one-hot key contributes nothing to the score until
//!   it has been observed [`MIN_OBSERVATIONS`] times, so a 1–2 example sender/domain one-hot
//!   can never swing the verdict on its own;
//! - **persistence** — when a path is configured the learned state is loaded at construction
//!   and rewritten after every update, so the classifier's online learning survives a host
//!   restart (the weights are a performance *cache*; the source of truth stays the feedback
//!   table, so a missing or corrupt file simply starts cold and re-warms from corrections).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use mailmate_common::error::MlError;
use mailmate_common::features::{
    CalibratedScores, FeatureVector, LabeledExample, SignalContribution,
};
use mailmate_ports::tier2_classifier::Tier2Classifier;

use crate::featurize::featurize;

/// The identity-calibration version tag this classifier stamps onto its scores. (A real
/// calibration table is a later, separately-versioned concern; the raw logistic output is
/// the honest v1 calibration.)
pub const CALIBRATION_VERSION: &str = "logreg-identity-v1";

const LEARNING_RATE: f64 = 0.2;
/// L2 weight-decay coefficient: every SGD step shrinks each touched weight toward zero, so a
/// feature seen in only a handful of corrections cannot accumulate an unbounded weight.
const L2: f64 = 1e-3;
/// Per-feature confidence floor: a feature contributes to the score only once it has been
/// observed at least this many times. Below the floor a freshly-seen one-hot is masked to a
/// zero contribution — the bias and well-supported features still move the verdict.
const MIN_OBSERVATIONS: u32 = 3;

#[derive(Debug, Default)]
struct Model {
    weights: BTreeMap<String, f64>,
    /// How many labelled examples have touched each feature key — the confidence-floor counter.
    counts: BTreeMap<String, u32>,
    bias: f64,
}

/// The on-disk form of a trained model: the labels it discriminates plus its learned state.
/// A file trained on different labels is treated as not-ours (loaded fresh), so the weights
/// can never be silently mislabelled.
#[derive(Debug, Serialize, Deserialize)]
struct PersistedModel {
    positive_label: String,
    negative_label: String,
    weights: BTreeMap<String, f64>,
    counts: BTreeMap<String, u32>,
    bias: f64,
}

/// An online binary logistic-regression classifier over two labels.
#[derive(Debug)]
pub struct LogisticRegressionClassifier {
    positive_label: String,
    negative_label: String,
    model: Mutex<Model>,
    /// When set, the model is loaded from here at construction and (best-effort) rewritten
    /// after every update; `None` keeps the model purely in-memory.
    path: Option<PathBuf>,
}

impl LogisticRegressionClassifier {
    /// A fresh, in-memory classifier discriminating `positive_label` from `negative_label`.
    #[must_use]
    pub fn new(positive_label: impl Into<String>, negative_label: impl Into<String>) -> Self {
        Self {
            positive_label: positive_label.into(),
            negative_label: negative_label.into(),
            model: Mutex::new(Model::default()),
            path: None,
        }
    }

    /// A classifier whose learned weights persist to `path`: loaded at construction (when the
    /// file exists and matches these labels) and rewritten after every online update. This is
    /// what keeps "a loadable model the classifier actually uses" continuously true across host
    /// restarts.
    #[must_use]
    pub fn with_persistence(
        positive_label: impl Into<String>,
        negative_label: impl Into<String>,
        path: PathBuf,
    ) -> Self {
        let positive_label = positive_label.into();
        let negative_label = negative_label.into();
        let model = load_model(&path, &positive_label, &negative_label).unwrap_or_default();
        Self {
            positive_label,
            negative_label,
            model: Mutex::new(model),
            path: Some(path),
        }
    }

    fn probability(model: &Model, features: &[(String, f64)]) -> f64 {
        Self::score(model, features).0
    }

    /// The calibrated probability **and** each feature's signed contribution toward the positive
    /// label (`weight × value` after the confidence floor). One pass computes both, so the
    /// explanation is the literal arithmetic that produced the score — no post-hoc reconstruction.
    fn score(model: &Model, features: &[(String, f64)]) -> (f64, Vec<(String, f64, f64)>) {
        let mut terms = Vec::with_capacity(features.len());
        let mut z = model.bias;
        for (key, x) in features {
            // A feature below its confidence floor contributes nothing to the score yet.
            let term = if model.counts.get(key).copied().unwrap_or(0) < MIN_OBSERVATIONS {
                0.0
            } else {
                model.weights.get(key).copied().unwrap_or(0.0) * x
            };
            z += term;
            terms.push((key.clone(), *x, term));
        }
        (sigmoid(z), terms)
    }

    fn snapshot(&self, model: &Model) -> PersistedModel {
        PersistedModel {
            positive_label: self.positive_label.clone(),
            negative_label: self.negative_label.clone(),
            weights: model.weights.clone(),
            counts: model.counts.clone(),
            bias: model.bias,
        }
    }
}

/// Load a persisted model, returning `None` (start fresh) on a missing/corrupt file or a
/// label mismatch — every failure path is non-fatal because the feedback table can re-warm it.
fn load_model(path: &Path, positive: &str, negative: &str) -> Option<Model> {
    let bytes = std::fs::read(path).ok()?;
    let persisted: PersistedModel = serde_json::from_slice(&bytes).ok()?;
    if persisted.positive_label != positive || persisted.negative_label != negative {
        return None;
    }
    Some(Model {
        weights: persisted.weights,
        counts: persisted.counts,
        bias: persisted.bias,
    })
}

/// Atomically persist a model snapshot (tmp-write + rename), private to the owner (0600 on
/// unix) since learned weights can encode signal derived from the user's mail.
fn persist_model(path: &Path, model: &PersistedModel) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(model)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    write_private(&tmp, &json)?;
    std::fs::rename(&tmp, path)
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

fn sigmoid(z: f64) -> f64 {
    1.0 / (1.0 + (-z).exp())
}

#[async_trait]
impl Tier2Classifier for LogisticRegressionClassifier {
    async fn predict(&self, features: FeatureVector) -> Result<CalibratedScores, MlError> {
        let inputs = featurize(&features);
        let model = self.model.lock().unwrap_or_else(PoisonError::into_inner);
        let (p, terms) = Self::score(&model, &inputs);
        let mut scores = BTreeMap::new();
        scores.insert(self.positive_label.clone(), p);
        scores.insert(self.negative_label.clone(), 1.0 - p);
        // Orient each contribution toward the top label so a positive weight always reads as
        // "this supported the verdict". Terms below the floor are masked to 0 and dropped here
        // (they explain nothing yet); the spine keeps only contributions that moved the score.
        let top_is_positive = p >= 0.5;
        let contributions = terms
            .into_iter()
            .filter(|(_, _, term)| term.abs() > f64::EPSILON)
            .map(|(key, value, term)| SignalContribution {
                key,
                value,
                signed_weight: if top_is_positive { term } else { -term },
            })
            .collect();
        Ok(CalibratedScores {
            scores,
            calibration_version: CALIBRATION_VERSION.to_owned(),
            contributions,
        })
    }

    async fn update(&self, labeled: LabeledExample) -> Result<(), MlError> {
        let inputs = featurize(&labeled.features);
        let target = if labeled.label == self.positive_label {
            1.0
        } else if labeled.label == self.negative_label {
            0.0
        } else {
            return Err(MlError::InvalidFeatures(format!(
                "label {:?} is neither {:?} nor {:?}",
                labeled.label, self.positive_label, self.negative_label
            )));
        };

        let snapshot = {
            let mut model = self.model.lock().unwrap_or_else(PoisonError::into_inner);
            let p = Self::probability(&model, &inputs);
            let gradient = p - target; // dLoss/dz for logistic loss
            for (key, x) in &inputs {
                *model.counts.entry(key.clone()).or_insert(0) += 1;
                let w = model.weights.entry(key.clone()).or_insert(0.0);
                // SGD step on the logistic loss, with L2 weight-decay.
                *w -= LEARNING_RATE * (gradient * x + L2 * *w);
            }
            model.bias -= LEARNING_RATE * gradient;
            self.path.as_ref().map(|_| self.snapshot(&model))
        };
        // Best-effort persistence outside the lock: the weights are a cache rebuildable from
        // the feedback table, so a write failure is non-fatal to the already-applied update.
        if let (Some(path), Some(snap)) = (self.path.as_ref(), snapshot) {
            let _ = persist_model(path, &snap);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use mailmate_common::features::FeatureValue;

    fn features(pairs: &[(&str, FeatureValue)]) -> FeatureVector {
        let mut fv = FeatureVector::new();
        for (k, v) in pairs {
            fv.insert(*k, v.clone());
        }
        fv
    }

    fn spam_clf() -> LogisticRegressionClassifier {
        LogisticRegressionClassifier::new("spam", "ham")
    }

    #[test]
    fn untrained_model_is_indifferent_and_deterministic() {
        let clf = spam_clf();
        let fv = features(&[("has_link", FeatureValue::Bool(true))]);
        let a = block_on(clf.predict(fv.clone())).unwrap();
        let b = block_on(clf.predict(fv)).unwrap();
        assert_eq!(a.scores, b.scores, "same input → same output");
        assert!((a.scores["spam"] - 0.5).abs() < 1e-9, "untrained → 0.5");
        assert_eq!(a.calibration_version, CALIBRATION_VERSION);
    }

    #[test]
    fn online_training_raises_confidence_for_the_learned_label() {
        let clf = spam_clf();
        let spammy = features(&[
            ("has_link", FeatureValue::Bool(true)),
            (
                "sender",
                FeatureValue::Text("spammer@bad.example".to_owned()),
            ),
        ]);
        let before = block_on(clf.predict(spammy.clone())).unwrap().scores["spam"];
        for _ in 0..40 {
            block_on(clf.update(LabeledExample {
                features: spammy.clone(),
                label: "spam".to_owned(),
            }))
            .unwrap();
        }
        let after = block_on(clf.predict(spammy)).unwrap().scores["spam"];
        assert!(
            after > before + 0.2,
            "confidence should rise: {before} -> {after}"
        );
        assert!(after > 0.5);
    }

    #[test]
    fn predict_exposes_the_signed_contributions_that_produced_the_score() {
        let clf = spam_clf();
        let spammy = features(&[("has_link", FeatureValue::Bool(true))]);
        // Train past the confidence floor so the feature actually contributes.
        for _ in 0..20 {
            block_on(clf.update(LabeledExample {
                features: spammy.clone(),
                label: "spam".to_owned(),
            }))
            .unwrap();
        }
        let scored = block_on(clf.predict(spammy)).unwrap();
        // The verdict is spam (the positive label) and the trained feature is the reason, with a
        // contribution oriented toward the verdict (positive supports it).
        assert!(scored.scores["spam"] > 0.5);
        let c = scored
            .contributions
            .iter()
            .find(|c| c.key == "has_link")
            .expect("the trained feature appears as a contribution");
        assert!(
            c.signed_weight > 0.0,
            "the feature supported the spam verdict: {}",
            c.signed_weight
        );

        // A ham verdict flips the orientation: the same feature now argues *against* the (ham)
        // verdict, so its contribution toward the top label is negative.
        let ham = spam_clf();
        let newsletter = features(&[("newsletter", FeatureValue::Bool(true))]);
        for _ in 0..20 {
            block_on(ham.update(LabeledExample {
                features: newsletter.clone(),
                label: "spam".to_owned(),
            }))
            .unwrap();
        }
        // Predict on a vector whose only trained feature pushes toward spam, but where the score
        // still lands on ham would be contradictory; instead assert the orientation invariant on
        // the spam-trained model directly: top label positive ⇒ supportive features positive.
        let scored2 = block_on(ham.predict(newsletter)).unwrap();
        let top_is_spam = scored2.scores["spam"] >= 0.5;
        let nl = scored2
            .contributions
            .iter()
            .find(|c| c.key == "newsletter")
            .unwrap();
        assert_eq!(
            nl.signed_weight > 0.0,
            top_is_spam,
            "a feature is positive iff it agrees with the top label"
        );
    }

    #[test]
    fn ham_examples_push_the_score_down() {
        let clf = spam_clf();
        let fv = features(&[("newsletter", FeatureValue::Bool(true))]);
        for _ in 0..40 {
            block_on(clf.update(LabeledExample {
                features: fv.clone(),
                label: "ham".to_owned(),
            }))
            .unwrap();
        }
        assert!(block_on(clf.predict(fv)).unwrap().scores["spam"] < 0.3);
    }

    #[test]
    fn an_unknown_label_is_rejected() {
        let clf = spam_clf();
        let err = block_on(clf.update(LabeledExample {
            features: FeatureVector::new(),
            label: "phishing".to_owned(),
        }))
        .unwrap_err();
        assert!(matches!(err, MlError::InvalidFeatures(_)), "got {err:?}");
    }

    fn unique_temp_path(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{tag}_{}_{n}.json", std::process::id()))
    }

    #[test]
    fn weights_persist_across_a_reload() {
        let path = unique_temp_path("mm_tier2_persist");
        let spammy = features(&[
            ("has_link", FeatureValue::Bool(true)),
            (
                "sender",
                FeatureValue::Text("spammer@bad.example".to_owned()),
            ),
        ]);
        {
            let clf = LogisticRegressionClassifier::with_persistence("spam", "ham", path.clone());
            for _ in 0..40 {
                block_on(clf.update(LabeledExample {
                    features: spammy.clone(),
                    label: "spam".to_owned(),
                }))
                .unwrap();
            }
            assert!(path.exists(), "an update must write the weights file");
        } // drop the classifier — its only state now lives on disk

        // A brand-new classifier loading the same path must predict the LEARNED score, not the
        // cold 0.5 — i.e. the online learning survived the "restart".
        let reloaded = LogisticRegressionClassifier::with_persistence("spam", "ham", path.clone());
        let score = block_on(reloaded.predict(spammy)).unwrap().scores["spam"];
        assert!(score > 0.7, "reloaded model kept its learning: {score}");

        // A file trained on different labels is not ours: load fresh rather than mislabel.
        let mismatched =
            LogisticRegressionClassifier::with_persistence("urgent", "normal", path.clone());
        let fresh = block_on(mismatched.predict(FeatureVector::new()))
            .unwrap()
            .scores["urgent"];
        assert!(
            (fresh - 0.5).abs() < 1e-9,
            "label mismatch ⇒ cold start: {fresh}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_rare_one_hot_below_the_floor_cannot_swing_the_verdict() {
        let clf = spam_clf();
        let rare = features(&[(
            "sender_domain",
            FeatureValue::Text("rare.example".to_owned()),
        )]);
        let empty = FeatureVector::new();

        // Two spam observations of a one-hot: still below the 3-observation floor, so it must
        // contribute nothing beyond what the (globally-moved) bias already does.
        for _ in 0..2 {
            block_on(clf.update(LabeledExample {
                features: rare.clone(),
                label: "spam".to_owned(),
            }))
            .unwrap();
        }
        let rare_score = block_on(clf.predict(rare.clone())).unwrap().scores["spam"];
        let empty_score = block_on(clf.predict(empty.clone())).unwrap().scores["spam"];
        assert!(
            (rare_score - empty_score).abs() < 1e-9,
            "below the floor the one-hot adds nothing: {rare_score} vs {empty_score}"
        );

        // A third observation crosses the floor — now the feature is trusted and moves the score.
        block_on(clf.update(LabeledExample {
            features: rare.clone(),
            label: "spam".to_owned(),
        }))
        .unwrap();
        let rare_after = block_on(clf.predict(rare)).unwrap().scores["spam"];
        let empty_after = block_on(clf.predict(empty)).unwrap().scores["spam"];
        assert!(
            rare_after > empty_after + 1e-6,
            "above the floor the one-hot contributes: {rare_after} vs {empty_after}"
        );
    }

    #[test]
    fn l2_decay_keeps_weights_bounded_under_repetition() {
        let clf = spam_clf();
        let fv = features(&[("x", FeatureValue::Bool(true))]);
        for _ in 0..5000 {
            block_on(clf.update(LabeledExample {
                features: fv.clone(),
                label: "spam".to_owned(),
            }))
            .unwrap();
        }
        // It still learned the label …
        assert!(block_on(clf.predict(fv)).unwrap().scores["spam"] > 0.9);
        // … but no weight ran away: L2 decay + logistic saturation keep every weight finite.
        let model = clf.model.lock().unwrap();
        for (k, w) in &model.weights {
            assert!(w.abs() < 50.0, "weight {k} should stay bounded, got {w}");
        }
    }
}
