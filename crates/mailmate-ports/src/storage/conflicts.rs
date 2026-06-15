//! The conflict repository port: persist detected rule-vs-rule conflicts and drive their
//! resolution state.
//!
//! A [`RuleConflictRecord`] is append-only at creation; only its [`ConflictStatus`] is
//! mutated as a human resolves or dismisses it. `RuleConflict` (the transient candidate
//! check) is *not* stored — only conflicts found between two live rules are recorded here.

use async_trait::async_trait;

use mailmate_common::conflict::{ConflictStatus, RuleConflictRecord};
use mailmate_common::error::StorageError;
use mailmate_common::ids::ConflictId;
use mailmate_common::time::Timestamp;

/// Persistence for recorded rule conflicts.
#[async_trait]
pub trait ConflictRepository: Send + Sync {
    /// Record a detected conflict (status `Open`). Returns its id.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn append(&self, conflict: RuleConflictRecord) -> Result<ConflictId, StorageError>;

    /// Every conflict still `Open`, newest first.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn list_open(&self) -> Result<Vec<RuleConflictRecord>, StorageError>;

    /// Transition a conflict's resolution state, recording the resolution time when supplied.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn set_status(
        &self,
        id: &ConflictId,
        status: ConflictStatus,
        resolved_at: Option<Timestamp>,
    ) -> Result<(), StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn ConflictRepository) {}
        let _ = takes as fn(&dyn ConflictRepository);
    }
}
