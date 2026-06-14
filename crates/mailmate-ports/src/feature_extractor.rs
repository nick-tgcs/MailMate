//! The feature-extractor port: a deterministic message → feature-vector function.

use mailmate_common::features::FeatureVector;
use mailmate_common::mail::MessageData;

/// Extracts the deterministic, non-body feature set the Tier-2 classifier consumes and
/// crystallization back-tests replay.
///
/// Implementations MUST be pure: the same `MessageData` yields an identical
/// `FeatureVector` every time, with no I/O. That purity is the contract that lets a
/// learned trait's back-test be reproducible.
pub trait FeatureExtractor: Send + Sync {
    /// Extract features from a message.
    fn extract(&self, msg: &MessageData) -> FeatureVector;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn FeatureExtractor) {}
        let _ = takes as fn(&dyn FeatureExtractor);
    }
}
