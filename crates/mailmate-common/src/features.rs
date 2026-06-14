//! Deterministic features and classifier I/O.
//!
//! A [`FeatureVector`] is the pure, non-body feature set the `FeatureExtractor`
//! produces and the `Tier2Classifier` consumes. It is an ordered map so it is stable
//! and hashable — which is what lets crystallization back-tests replay identically.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A single feature value. `untagged` so it serializes as the bare scalar/JSON.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum FeatureValue {
    /// Boolean feature.
    Bool(bool),
    /// Numeric feature.
    Number(f64),
    /// Textual feature.
    Text(String),
    /// Structured feature (escape hatch).
    Json(serde_json::Value),
}

/// An ordered, deterministic feature set keyed by feature name.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(transparent)]
pub struct FeatureVector {
    /// The features, ordered by name for stable iteration and hashing.
    pub features: BTreeMap<String, FeatureValue>,
}

impl FeatureVector {
    /// An empty feature vector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or overwrite a feature.
    pub fn insert(&mut self, name: impl Into<String>, value: FeatureValue) {
        self.features.insert(name.into(), value);
    }

    /// Look up a feature by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&FeatureValue> {
        self.features.get(name)
    }

    /// Number of features.
    #[must_use]
    pub fn len(&self) -> usize {
        self.features.len()
    }

    /// Whether the vector has no features.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }
}

/// A calibrated probability distribution over labels, produced by `Tier2Classifier`.
///
/// `calibration_version` is carried so a score can be tied to the calibration table
/// that produced it (everything versioned, per the cascade design).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct CalibratedScores {
    /// Label → calibrated probability in `[0, 1]`.
    pub scores: BTreeMap<String, f64>,
    /// Identifier of the calibration table used.
    pub calibration_version: String,
}

/// A labelled training example fed to `Tier2Classifier::update`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LabeledExample {
    /// The features observed.
    pub features: FeatureVector,
    /// The ground-truth label (typically a user correction).
    pub label: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_vector_iterates_in_name_order() {
        let mut fv = FeatureVector::new();
        fv.insert("zeta", FeatureValue::Bool(true));
        fv.insert("alpha", FeatureValue::Number(1.0));
        let names: Vec<&str> = fv.features.keys().map(String::as_str).collect();
        assert_eq!(names, vec!["alpha", "zeta"], "BTreeMap keeps name order");
        assert_eq!(fv.len(), 2);
        assert!(!fv.is_empty());
    }

    #[test]
    fn feature_value_serializes_untagged() {
        assert_eq!(
            serde_json::to_string(&FeatureValue::Bool(true)).unwrap(),
            "true"
        );
        assert_eq!(
            serde_json::to_string(&FeatureValue::Text("x".to_owned())).unwrap(),
            "\"x\""
        );
    }

    #[test]
    fn feature_vector_round_trips_and_is_stable() {
        let mut fv = FeatureVector::new();
        fv.insert("sender_in_contacts", FeatureValue::Bool(false));
        fv.insert("subject_len", FeatureValue::Number(12.0));
        let a = serde_json::to_string(&fv).unwrap();
        let b = serde_json::to_string(&fv).unwrap();
        assert_eq!(a, b, "same vector serializes identically");
        let back: FeatureVector = serde_json::from_str(&a).unwrap();
        assert_eq!(back, fv);
    }
}
