//! The message repository port: persist intaken messages and their non-body features,
//! drive the background-classification queue, and read messages back.

use async_trait::async_trait;

use mailmate_common::error::StorageError;
use mailmate_common::features::FeatureVector;
use mailmate_common::ids::MessageId;
use mailmate_common::message::{ClassificationStatus, NewMessage, StoredFeature, StoredMessage};

/// Persistence for messages and their features.
///
/// [`insert`](MessageRepository::insert) enforces the body-retention privacy default: the
/// readable `body_text` is written only when the message's retention level allows it. The
/// repository — never the caller — makes that call, so a body cannot be persisted by
/// mistake.
#[async_trait]
pub trait MessageRepository: Send + Sync {
    /// Persist a new message (entering the queue as `pending`); returns its id.
    ///
    /// # Errors
    /// [`StorageError`] on a constraint violation or backend failure.
    async fn insert(&self, message: NewMessage) -> Result<MessageId, StorageError>;

    /// Fetch a message by id, or `None` if absent.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn get(&self, id: &MessageId) -> Result<Option<StoredMessage>, StorageError>;

    /// Move a message to a new background-queue state.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn set_classification_status(
        &self,
        id: &MessageId,
        status: ClassificationStatus,
    ) -> Result<(), StorageError>;

    /// All messages currently in `status`, oldest first (the queue-drain order).
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn list_by_status(
        &self,
        status: ClassificationStatus,
    ) -> Result<Vec<StoredMessage>, StorageError>;

    /// Append the non-body features of a message.
    ///
    /// # Errors
    /// [`StorageError::Constraint`] if the message does not exist (FK), or other
    /// [`StorageError`] on a backend failure.
    async fn add_features(
        &self,
        message_id: &MessageId,
        features: &FeatureVector,
    ) -> Result<(), StorageError>;

    /// Read a message's stored features, ordered by feature name.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn get_features(
        &self,
        message_id: &MessageId,
    ) -> Result<Vec<StoredFeature>, StorageError>;

    /// The **down-level purge**: NULL every retained `body_text` and clear `body_retained`,
    /// returning how many bodies were purged. Called when the user lowers retention below
    /// body-retention so "turn the dial down and the bodies are gone" is true immediately.
    /// `body_hash` (identity/dedup) is kept — a hash is not the body.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn purge_bodies(&self) -> Result<u64, StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn MessageRepository) {}
        let _ = takes as fn(&dyn MessageRepository);
    }
}
