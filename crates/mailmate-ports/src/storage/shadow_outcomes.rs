//! The shadow-outcome repository port: append live shadow firings and read them back per
//! rule, the source `RuleOutcome` derives shadow precision from.

use async_trait::async_trait;

use mailmate_common::error::StorageError;
use mailmate_common::ids::{RuleId, ShadowOutcomeId};
use mailmate_common::shadow::ShadowOutcomeRow;

/// Append-and-read persistence for shadow firings.
#[async_trait]
pub trait ShadowOutcomeRepository: Send + Sync {
    /// Record one shadow firing; returns its id.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn append(&self, row: ShadowOutcomeRow) -> Result<ShadowOutcomeId, StorageError>;

    /// All shadow firings recorded for `rule_id`, oldest first.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn list_for_rule(&self, rule_id: &RuleId) -> Result<Vec<ShadowOutcomeRow>, StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn ShadowOutcomeRepository) {}
        let _ = takes as fn(&dyn ShadowOutcomeRepository);
    }
}
