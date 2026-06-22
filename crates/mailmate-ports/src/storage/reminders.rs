//! The reminder repository port: persist durable notify-only remind-me / snooze timers and
//! drain the ones that have come due. The drain is **bounded** by an explicit `limit` (the batch
//! cap), so even a large catch-up backlog after a long offline period nudges in capped batches
//! rather than emitting an unbounded storm in one tick.

use async_trait::async_trait;

use mailmate_common::error::StorageError;
use mailmate_common::ids::ReminderId;
use mailmate_common::reminder::{NewReminder, Reminder};
use mailmate_common::time::Timestamp;

/// Persistence for durable reminders.
#[async_trait]
pub trait ReminderRepository: Send + Sync {
    /// Arm a new reminder; returns its id.
    ///
    /// # Errors
    /// [`StorageError`] on a constraint violation or backend failure.
    async fn create(&self, reminder: NewReminder) -> Result<ReminderId, StorageError>;

    /// The pending reminders due at or before `now`, soonest first, capped at `limit` rows — the
    /// bounded drain batch. A `limit` of 0 returns nothing.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn list_due(&self, now: Timestamp, limit: usize)
        -> Result<Vec<Reminder>, StorageError>;

    /// Mark a reminder fired (terminal): it will never be selected by [`list_due`] again.
    ///
    /// [`list_due`]: ReminderRepository::list_due
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn mark_fired(&self, id: &ReminderId, fired_at: Timestamp)
        -> Result<(), StorageError>;

    /// Reschedule a reminder to a new due time and re-arm it to `pending` — this is **snooze**
    /// (push a still-pending one out) and **un-fire** (bring a fired one back) in one operation.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn reschedule(&self, id: &ReminderId, due_at: Timestamp) -> Result<(), StorageError>;

    /// Cancel a reminder (terminal): the user dismissed it before it fired.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn cancel(&self, id: &ReminderId) -> Result<(), StorageError>;

    /// Fetch a reminder by id, or `None` if absent.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn get(&self, id: &ReminderId) -> Result<Option<Reminder>, StorageError>;

    /// The still-pending reminders, soonest-due first, capped at `limit` — the UI list.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn list_pending(&self, limit: usize) -> Result<Vec<Reminder>, StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn ReminderRepository) {}
        let _ = takes as fn(&dyn ReminderRepository);
    }
}
