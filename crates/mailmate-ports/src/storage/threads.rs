//! The thread repository port: persist conversation threads and accumulate their
//! per-message counters.

use async_trait::async_trait;

use mailmate_common::error::StorageError;
use mailmate_common::ids::ThreadId;
use mailmate_common::thread::Thread;
use mailmate_common::time::Timestamp;

/// Persistence for conversation threads.
#[async_trait]
pub trait ThreadRepository: Send + Sync {
    /// Persist a new thread.
    ///
    /// # Errors
    /// [`StorageError`] on a constraint violation or backend failure.
    async fn insert(&self, thread: Thread) -> Result<(), StorageError>;

    /// Fetch a thread by id, or `None` if absent.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn get(&self, id: &ThreadId) -> Result<Option<Thread>, StorageError>;

    /// Record that another message was seen in the thread: bump `message_count` and set
    /// `last_seen_at`. (An explicit counter bump, not an upsert — the seam needs none.)
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn record_message_seen(
        &self,
        id: &ThreadId,
        seen_at: Timestamp,
    ) -> Result<(), StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn ThreadRepository) {}
        let _ = takes as fn(&dyn ThreadRepository);
    }
}
