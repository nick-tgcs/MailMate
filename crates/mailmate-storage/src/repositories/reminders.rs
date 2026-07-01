//! SQLite implementation of [`ReminderRepository`]: durable notify-only remind-me / snooze
//! timers, drained in bounded batches.
//!
//! `due_at` is stored at whole-second RFC-3339 UTC precision (like `workflow_instances.next_due_at`)
//! so the drain's `due_at <= now` predicate is an exact lexicographic TEXT compare served by the
//! `idx_reminders_status_due` index. Firing is idempotent: `mark_fired` moves a row to the
//! terminal `fired` status, so a re-drain after a crash never re-selects it.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Row};

use mailmate_common::error::StorageError;
use mailmate_common::ids::{AccountId, MessageId, ReminderId, ThreadId};
use mailmate_common::reminder::{NewReminder, Reminder, ReminderStatus};
use mailmate_common::time::Timestamp;
use mailmate_ports::storage::reminders::ReminderRepository;

use crate::backend::{map_rusqlite, SqliteBackend};
use crate::repositories::convert::{ts_from_db, ts_to_db};

const COLUMNS: &str =
    "id, message_id, thread_id, account_id, title, note, due_at, status, created_at, fired_at";

/// SQLite implementation of [`ReminderRepository`].
pub struct SqliteReminderRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteReminderRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

fn row_to_reminder(row: &Row<'_>) -> Result<Reminder, StorageError> {
    let status: String = row.get(7).map_err(map_rusqlite)?;
    let due_at: String = row.get(6).map_err(map_rusqlite)?;
    let created_at: String = row.get(8).map_err(map_rusqlite)?;
    let fired_at: Option<String> = row.get(9).map_err(map_rusqlite)?;
    Ok(Reminder {
        id: ReminderId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        message_id: row
            .get::<_, Option<String>>(1)
            .map_err(map_rusqlite)?
            .map(MessageId::from),
        thread_id: row
            .get::<_, Option<String>>(2)
            .map_err(map_rusqlite)?
            .map(ThreadId::from),
        account_id: row
            .get::<_, Option<String>>(3)
            .map_err(map_rusqlite)?
            .map(AccountId::from),
        title: row.get(4).map_err(map_rusqlite)?,
        note: row.get(5).map_err(map_rusqlite)?,
        due_at: ts_from_db(&due_at)?,
        status: ReminderStatus::from_db_str(&status)
            .ok_or_else(|| StorageError::Backend(format!("unknown reminder status {status:?}")))?,
        created_at: ts_from_db(&created_at)?,
        fired_at: fired_at.as_deref().map(ts_from_db).transpose()?,
    })
}

#[async_trait]
impl ReminderRepository for SqliteReminderRepository {
    async fn create(&self, reminder: NewReminder) -> Result<ReminderId, StorageError> {
        let id = reminder.id.clone();
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO reminders \
                 (id, message_id, thread_id, account_id, title, note, due_at, status, created_at, fired_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,NULL)",
                params![
                    reminder.id.as_str(),
                    reminder.message_id.as_ref().map(MessageId::as_str),
                    reminder.thread_id.as_ref().map(ThreadId::as_str),
                    reminder.account_id.as_ref().map(AccountId::as_str),
                    reminder.title,
                    reminder.note,
                    ts_to_db(reminder.due_at.floor_to_seconds()),
                    ReminderStatus::Pending.as_str(),
                    ts_to_db(reminder.created_at),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn list_due(&self, now: Timestamp, limit: usize) -> Result<Vec<Reminder>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let now_db = ts_to_db(now.floor_to_seconds());
        self.backend.with_conn(|conn| {
            let sql = format!(
                "SELECT {COLUMNS} FROM reminders \
                 WHERE status = 'pending' AND due_at <= ?1 \
                 ORDER BY due_at ASC, id ASC LIMIT ?2"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt
                .query(params![now_db, limit as i64])
                .map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_reminder(row)?);
            }
            Ok(out)
        })
    }

    async fn mark_fired(&self, id: &ReminderId, fired_at: Timestamp) -> Result<(), StorageError> {
        self.backend.with_conn(|conn| {
            // Re-check the fire condition ATOMICALLY at write time so a concurrent snooze or
            // cancel that landed between the drain's `list_due` read and this write WINS — the
            // drain must not resurrect a cancelled reminder, nor clobber a fresh snooze by forcing
            // it terminal. (The host's periodic drain runs on a separate thread from the request
            // handlers, so this interleaving is real, not theoretical.) `status = 'pending'`
            // rejects a cancel/fire; `due_at <= fired_at` rejects a snooze that pushed the due time
            // into the future (the drain passes its own `now` as `fired_at`, so this is exactly the
            // due condition `list_due` selected on). A snooze to a still-past time stays due and is
            // fired, which is correct.
            let fired_db = ts_to_db(fired_at.floor_to_seconds());
            conn.execute(
                "UPDATE reminders SET status = 'fired', fired_at = ?2 \
                 WHERE id = ?1 AND status = 'pending' AND due_at <= ?2",
                params![id.as_str(), fired_db],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }

    async fn reschedule(&self, id: &ReminderId, due_at: Timestamp) -> Result<(), StorageError> {
        self.backend.with_conn(|conn| {
            conn.execute(
                "UPDATE reminders SET due_at = ?2, status = 'pending', fired_at = NULL WHERE id = ?1",
                params![id.as_str(), ts_to_db(due_at.floor_to_seconds())],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }

    async fn cancel(&self, id: &ReminderId) -> Result<(), StorageError> {
        self.backend.with_conn(|conn| {
            conn.execute(
                "UPDATE reminders SET status = 'cancelled' WHERE id = ?1",
                params![id.as_str()],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }

    async fn get(&self, id: &ReminderId) -> Result<Option<Reminder>, StorageError> {
        self.backend.with_conn(|conn| {
            let sql = format!("SELECT {COLUMNS} FROM reminders WHERE id = ?1");
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![id.as_str()]).map_err(map_rusqlite)?;
            match rows.next().map_err(map_rusqlite)? {
                Some(row) => Ok(Some(row_to_reminder(row)?)),
                None => Ok(None),
            }
        })
    }

    async fn list_pending(&self, limit: usize) -> Result<Vec<Reminder>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.backend.with_conn(|conn| {
            let sql = format!(
                "SELECT {COLUMNS} FROM reminders WHERE status = 'pending' \
                 ORDER BY due_at ASC, id ASC LIMIT ?1"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![limit as i64]).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_reminder(row)?);
            }
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    use crate::backend::{open_and_migrate, StorageConfig};

    fn db() -> Arc<SqliteBackend> {
        open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap()
    }

    fn at(s: &str) -> Timestamp {
        Timestamp::parse_rfc3339(s).unwrap()
    }

    fn new_reminder(id: &str, due: &str) -> NewReminder {
        NewReminder {
            id: ReminderId::from(id),
            message_id: Some(MessageId::from("msg_1")),
            thread_id: None,
            account_id: Some(AccountId::from("acct")),
            title: format!("remind {id}"),
            note: Some("reply with the quote".to_owned()),
            due_at: at(due),
            created_at: at("2026-06-22T00:00:00Z"),
        }
    }

    #[test]
    fn a_due_reminder_is_drained_then_never_fires_again() {
        let backend = db();
        let repo = SqliteReminderRepository::new(backend);
        block_on(repo.create(new_reminder("rem_1", "2026-06-22T09:00:00Z"))).unwrap();

        // Before due: nothing.
        let before = block_on(repo.list_due(at("2026-06-22T08:59:59Z"), 10)).unwrap();
        assert!(before.is_empty(), "not yet due");

        // At/after due: exactly one, carrying its payload.
        let due = block_on(repo.list_due(at("2026-06-22T09:00:00Z"), 10)).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].title, "remind rem_1");
        assert_eq!(due[0].note.as_deref(), Some("reply with the quote"));

        // Fire it, then a re-drain finds nothing (idempotent — no double-nudge).
        block_on(repo.mark_fired(&ReminderId::from("rem_1"), at("2026-06-22T09:00:01Z"))).unwrap();
        let after = block_on(repo.list_due(at("2026-06-22T10:00:00Z"), 10)).unwrap();
        assert!(after.is_empty(), "a fired reminder must not re-fire");
        let got = block_on(repo.get(&ReminderId::from("rem_1")))
            .unwrap()
            .unwrap();
        assert_eq!(got.status, ReminderStatus::Fired);
        assert!(got.fired_at.is_some());
    }

    #[test]
    fn snooze_pushes_a_due_reminder_back_out_and_re_arms_a_fired_one() {
        let backend = db();
        let repo = SqliteReminderRepository::new(backend);
        block_on(repo.create(new_reminder("rem_1", "2026-06-22T09:00:00Z"))).unwrap();

        // Snooze the still-pending reminder a day forward: no longer due now.
        block_on(repo.reschedule(&ReminderId::from("rem_1"), at("2026-06-23T09:00:00Z"))).unwrap();
        assert!(block_on(repo.list_due(at("2026-06-22T12:00:00Z"), 10))
            .unwrap()
            .is_empty());
        assert_eq!(
            block_on(repo.list_due(at("2026-06-23T09:00:00Z"), 10))
                .unwrap()
                .len(),
            1
        );

        // Fire then reschedule (un-fire): it becomes pending and drains again.
        block_on(repo.mark_fired(&ReminderId::from("rem_1"), at("2026-06-23T09:00:01Z"))).unwrap();
        block_on(repo.reschedule(&ReminderId::from("rem_1"), at("2026-06-24T09:00:00Z"))).unwrap();
        let got = block_on(repo.get(&ReminderId::from("rem_1")))
            .unwrap()
            .unwrap();
        assert_eq!(got.status, ReminderStatus::Pending);
        assert_eq!(got.fired_at, None, "re-arming clears the fired stamp");
    }

    #[test]
    fn list_due_is_bounded_by_the_batch_cap_and_orders_soonest_first() {
        let backend = db();
        let repo = SqliteReminderRepository::new(backend);
        for (i, due) in ["09:00", "09:01", "09:02", "09:03", "09:04"]
            .iter()
            .enumerate()
        {
            block_on(repo.create(new_reminder(
                &format!("rem_{i}"),
                &format!("2026-06-22T{due}:00Z"),
            )))
            .unwrap();
        }
        // All five are due, but the cap returns only the three soonest.
        let batch = block_on(repo.list_due(at("2026-06-22T10:00:00Z"), 3)).unwrap();
        assert_eq!(batch.len(), 3, "the batch cap bounds the drain");
        assert_eq!(batch[0].id.as_str(), "rem_0");
        assert_eq!(batch[2].id.as_str(), "rem_2");
        // A zero cap drains nothing.
        assert!(block_on(repo.list_due(at("2026-06-22T10:00:00Z"), 0))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn cancel_makes_a_reminder_terminal() {
        let backend = db();
        let repo = SqliteReminderRepository::new(backend);
        block_on(repo.create(new_reminder("rem_1", "2026-06-22T09:00:00Z"))).unwrap();
        block_on(repo.cancel(&ReminderId::from("rem_1"))).unwrap();
        assert!(block_on(repo.list_due(at("2026-06-22T10:00:00Z"), 10))
            .unwrap()
            .is_empty());
        assert!(block_on(repo.list_pending(10)).unwrap().is_empty());
        assert_eq!(
            block_on(repo.get(&ReminderId::from("rem_1")))
                .unwrap()
                .unwrap()
                .status,
            ReminderStatus::Cancelled
        );
    }

    #[test]
    fn a_concurrent_cancel_beats_the_drains_mark_fired() {
        // The drain read the reminder as due, but the user cancelled before mark_fired ran.
        // mark_fired must NOT resurrect the cancelled reminder to terminal-fired.
        let backend = db();
        let repo = SqliteReminderRepository::new(backend);
        block_on(repo.create(new_reminder("rem_1", "2026-06-22T09:00:00Z"))).unwrap();
        block_on(repo.cancel(&ReminderId::from("rem_1"))).unwrap();
        block_on(repo.mark_fired(&ReminderId::from("rem_1"), at("2026-06-22T09:00:01Z"))).unwrap();
        assert_eq!(
            block_on(repo.get(&ReminderId::from("rem_1")))
                .unwrap()
                .unwrap()
                .status,
            ReminderStatus::Cancelled,
            "a cancelled reminder must stay cancelled, not be forced fired by the drain"
        );
    }

    #[test]
    fn a_concurrent_snooze_into_the_future_beats_the_drains_mark_fired() {
        // The drain read the reminder as due at 09:00, but the user snoozed it to tomorrow before
        // mark_fired ran. mark_fired (with the drain's `now`=09:00:01) must NOT clobber the snooze.
        let backend = db();
        let repo = SqliteReminderRepository::new(backend);
        block_on(repo.create(new_reminder("rem_1", "2026-06-22T09:00:00Z"))).unwrap();
        block_on(repo.reschedule(&ReminderId::from("rem_1"), at("2026-06-23T09:00:00Z"))).unwrap();
        block_on(repo.mark_fired(&ReminderId::from("rem_1"), at("2026-06-22T09:00:01Z"))).unwrap();
        let got = block_on(repo.get(&ReminderId::from("rem_1")))
            .unwrap()
            .unwrap();
        assert_eq!(
            got.status,
            ReminderStatus::Pending,
            "the snooze wins — still pending"
        );
        assert_eq!(got.fired_at, None, "and it was not stamped fired");
        // It will fire at the new (snoozed) time.
        assert_eq!(
            block_on(repo.list_due(at("2026-06-23T09:00:00Z"), 10))
                .unwrap()
                .len(),
            1
        );
    }
}
