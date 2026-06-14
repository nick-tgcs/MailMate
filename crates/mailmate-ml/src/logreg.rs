//! An online binary logistic-regression [`Tier2Classifier`] — the lightweight, always-on
//! alternative to the Burn discriminative classifier.
//!
//! It featurizes a [`FeatureVector`] deterministically (bools → 0/1, numbers as-is, text →
//! one-hot `name=value` keys), maintains a sparse weight map, predicts a calibrated
//! probability via the logistic function, and learns online from each labelled example by
//! one SGD step on the logistic loss. Pure Rust, no model runtime — same input → same
//! output, fully testable without a GPU or Burn.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;

use mailmate_common::error::MlError;
use mailmate_common::features::{CalibratedScores, FeatureValue, FeatureVector, LabeledExample};
use mailmate_ports::tier2_classifier::Tier2Classifier;

/// The identity-calibration version tag this classifier stamps onto its scores. (A real
/// calibration table is a later, separately-versioned concern; the raw logistic output is
/// the honest v1 calibration.)
pub const CALIBRATION_VERSION: &str = "logreg-identity-v1";

const LEARNING_RATE: f64 = 0.2;

#[derive(Debug, Default)]
struct Model {
    weights: BTreeMap<String, f64>,
    bias: f64,
}

/// An online binary logistic-regression classifier over two labels.
#[derive(Debug)]
pub struct LogisticRegressionClassifier {
    positive_label: String,
    negative_label: String,
    model: Mutex<Model>,
}

impl LogisticRegressionClassifier {
    /// A fresh classifier discriminating `positive_label` from `negative_label`.
    #[must_use]
    pub fn new(positive_label: impl Into<String>, negative_label: impl Into<String>) -> Self {
        Self {
            positive_label: positive_label.into(),
            negative_label: negative_label.into(),
            model: Mutex::new(Model::default()),
        }
    }

    fn probability(model: &Model, features: &[(String, f64)]) -> f64 {
        let z = features
            .iter()
            .map(|(key, x)| model.weights.get(key).copied().unwrap_or(0.0) * x)
            .sum::<f64>()
            + model.bias;
        sigmoid(z)
    }
}

/// Deterministically turn a feature vector into `(key, value)` numeric inputs.
fn featurize(features: &FeatureVector) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for (name, value) in &features.features {
        match value {
            FeatureValue::Bool(b) => out.push((name.clone(), f64::from(*b))),
            FeatureValue::Number(n) => out.push((name.clone(), *n)),
            FeatureValue::Text(s) => out.push((format!("{name}={s}"), 1.0)),
            // Structured features are not linearly featurized here.
            FeatureValue::Json(_) => {}
        }
    }
    out
}

fn sigmoid(z: f64) -> f64 {
    1.0 / (1.0 + (-z).exp())
}

#[async_trait]
impl Tier2Classifier for LogisticRegressionClassifier {
    async fn predict(&self, features: FeatureVector) -> Result<CalibratedScores, MlError> {
        let inputs = featurize(&features);
        let model = self.model.lock().unwrap_or_else(PoisonError::into_inner);
        let p = Self::probability(&model, &inputs);
        let mut scores = BTreeMap::new();
        scores.insert(self.positive_label.clone(), p);
        scores.insert(self.negative_label.clone(), 1.0 - p);
        Ok(CalibratedScores {
            scores,
            calibration_version: CALIBRATION_VERSION.to_owned(),
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

        let mut model = self.model.lock().unwrap_or_else(PoisonError::into_inner);
        let p = Self::probability(&model, &inputs);
        let gradient = p - target; // dLoss/dz for logistic loss
        for (key, x) in &inputs {
            let w = model.weights.entry(key.clone()).or_insert(0.0);
            *w -= LEARNING_RATE * gradient * x;
        }
        model.bias -= LEARNING_RATE * gradient;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

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
}
