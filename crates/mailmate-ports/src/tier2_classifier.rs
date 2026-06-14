//! The Tier-2 classifier port: the cascade's cheap local model.

use async_trait::async_trait;

use mailmate_common::error::MlError;
use mailmate_common::features::{CalibratedScores, FeatureVector, LabeledExample};

/// The cascade's Tier-2 model: predict calibrated scores from deterministic features,
/// and learn from corrections.
///
/// The default adapter is a small Burn-trained discriminative classifier; an online
/// logistic-regression adapter and a deterministic mock are alternatives. Whatever the
/// adapter, gating thresholds on the *calibrated* score, never a raw logit.
#[async_trait]
pub trait Tier2Classifier: Send + Sync {
    /// Predict a calibrated score distribution for a feature vector.
    async fn predict(&self, features: FeatureVector) -> Result<CalibratedScores, MlError>;

    /// Incorporate a labelled example (online or batched by the adapter).
    async fn update(&self, labeled: LabeledExample) -> Result<(), MlError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn Tier2Classifier) {}
        let _ = takes as fn(&dyn Tier2Classifier);
    }
}
