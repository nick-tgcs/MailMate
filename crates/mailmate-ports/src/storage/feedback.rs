//! The per-task feedback repository port: one typed repository per feedback table
//! (classification, filing, …), each the sole writer of its task's signal.
//!
//! It is generic over [`TaskFeedbackKind`] — the marker that names a table's row type,
//! query type, evidence source, and id prefix. One mechanism, many tables: the adapter
//! provides one `impl FeedbackRepository<K>` per kind, and a `followup_feedback` kind
//! (Phase 11) plugs into this same generic with no new mechanism.

use async_trait::async_trait;

use mailmate_common::error::StorageError;
use mailmate_common::feedback::TaskFeedbackKind;
use mailmate_common::ids::FeedbackId;

/// Append-and-read persistence for one per-task feedback table.
#[async_trait]
pub trait FeedbackRepository<F: TaskFeedbackKind>: Send + Sync {
    /// Append one feedback row; returns its id.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn append(&self, row: F::Row) -> Result<FeedbackId, StorageError>;

    /// Read the rows matching `query`, newest first.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn query(&self, query: F::Query) -> Result<Vec<F::Row>, StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::feedback::ClassificationFeedback;

    #[test]
    fn port_is_object_safe_per_kind() {
        fn takes(_: &dyn FeedbackRepository<ClassificationFeedback>) {}
        let _ = takes as fn(&dyn FeedbackRepository<ClassificationFeedback>);
    }
}
