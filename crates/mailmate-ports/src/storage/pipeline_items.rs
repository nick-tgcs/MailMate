//! The pipeline-item repository port: persist tracked quotes/proposals, read them back by
//! id or thread, and update their stage. A deliberately minimal tracker, not a CRM.

use async_trait::async_trait;

use mailmate_common::error::StorageError;
use mailmate_common::ids::{PipelineItemId, ThreadId};
use mailmate_common::pipeline::{NewPipelineItem, PipelineItem, PipelineItemQuery, PipelineStage};

/// Append-and-read persistence for sales-pipeline items.
#[async_trait]
pub trait PipelineItemRepository: Send + Sync {
    /// Enroll a new tracked deal (stage `open`, `created_by = user`); returns its id. The
    /// repository stamps the id and the `created_at`/`updated_at`/`last_activity_at`
    /// timestamps.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn insert(&self, item: NewPipelineItem) -> Result<PipelineItemId, StorageError>;

    /// Fetch an item by id, or `None` if absent.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn get(&self, id: &PipelineItemId) -> Result<Option<PipelineItem>, StorageError>;

    /// All items anchored on `thread_id` (reply-exit looks an item up by its thread).
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn get_by_thread(&self, thread_id: &ThreadId) -> Result<Vec<PipelineItem>, StorageError>;

    /// Move an item to a new stage (also bumps `updated_at`/`last_activity_at`).
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn update_stage(
        &self,
        id: &PipelineItemId,
        stage: PipelineStage,
    ) -> Result<(), StorageError>;

    /// Read items matching `query`, newest first.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn query(&self, query: PipelineItemQuery) -> Result<Vec<PipelineItem>, StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn PipelineItemRepository) {}
        let _ = takes as fn(&dyn PipelineItemRepository);
    }
}
