//! The draft repository port: persist review-required drafts and advance their status.

use async_trait::async_trait;

use mailmate_common::draft::{DraftRecord, DraftStatus, NewDraft};
use mailmate_common::error::StorageError;
use mailmate_common::ids::DraftId;
use mailmate_common::time::Timestamp;

/// Persistence for generated drafts.
///
/// [`insert`](DraftRepository::insert) always persists `requires_review = 1`; the input
/// [`NewDraft`] cannot express otherwise, so the never-auto-send law holds at the storage
/// boundary.
#[async_trait]
pub trait DraftRepository: Send + Sync {
    /// Persist a new draft (always review-required); returns its id.
    ///
    /// # Errors
    /// [`StorageError`] on a constraint violation or backend failure.
    async fn insert(&self, draft: NewDraft) -> Result<DraftId, StorageError>;

    /// Fetch a draft by id, or `None` if absent.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn get(&self, id: &DraftId) -> Result<Option<DraftRecord>, StorageError>;

    /// Advance a draft's lifecycle status and `updated_at`.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn set_status(
        &self,
        id: &DraftId,
        status: DraftStatus,
        updated_at: Timestamp,
    ) -> Result<(), StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn DraftRepository) {}
        let _ = takes as fn(&dyn DraftRepository);
    }
}
