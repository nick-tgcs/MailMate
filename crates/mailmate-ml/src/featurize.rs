//! The one deterministic [`FeatureVector`] → numeric `(key, value)` mapping shared by every
//! Tier-2 adapter (the online logistic regression and the Burn-trained classifier), so a model
//! trained on these inputs and a model serving them agree key-for-key.

use mailmate_common::features::{FeatureValue, FeatureVector};

/// Deterministically turn a feature vector into `(key, value)` numeric inputs: bools → 0/1,
/// numbers as-is, text → a one-hot `name=value` key at 1.0. Structured (`Json`) features are
/// not linearly featurized.
#[must_use]
pub fn featurize(features: &FeatureVector) -> Vec<(String, f64)> {
    let mut out = Vec::with_capacity(features.features.len());
    for (name, value) in &features.features {
        match value {
            FeatureValue::Bool(b) => out.push((name.clone(), f64::from(*b))),
            FeatureValue::Number(n) => out.push((name.clone(), *n)),
            FeatureValue::Text(s) => out.push((format!("{name}={s}"), 1.0)),
            FeatureValue::Json(_) => {}
        }
    }
    out
}
