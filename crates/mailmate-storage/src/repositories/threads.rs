//! SQLite-backed [`ThreadRepository`].

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Row};

use mailmate_common::error::StorageError;
use mailmate_common::ids::{AccountId, ThreadId};
use mailmate_common::thread::Thread;
use mailmate_common::time::Timestamp;
use mailmate_ports::storage::threads::ThreadRepository;

use crate::backend::{map_rusqlite, SqliteBackend};
use crate::repositories::convert::{json_from_db, json_to_db, ts_from_db, ts_to_db};

const THREAD_COLUMNS: &str = "id, account_id, subject_root_normalized, participant_domains, \
     message_count, first_seen_at, last_seen_at, last_summary, last_summarized_at, created_at";

/// SQLite implementation of [`ThreadRepository`].
pub struct SqliteThreadRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteThreadRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

fn row_to_thread(row: &Row<'_>) -> Result<Thread, StorageError> {
    let participant_domains: String = row.get(3).map_err(map_rusqlite)?;
    let first_seen_at: String = row.get(5).map_err(map_rusqlite)?;
    let last_seen_at: String = row.get(6).map_err(map_rusqlite)?;
    let last_summarized_at: Option<String> = row.get(8).map_err(map_rusqlite)?;

    Ok(Thread {
        id: ThreadId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        account_id: AccountId::from(row.get::<_, String>(1).map_err(map_rusqlite)?),
        subject_root_normalized: row.get(2).map_err(map_rusqlite)?,
        participant_domains: json_from_db(&participant_domains)?,
        message_count: row.get(4).map_err(map_rusqlite)?,
        first_seen_at: ts_from_db(&first_seen_at)?,
        last_seen_at: ts_from_db(&last_seen_at)?,
        last_summary: row.get(7).map_err(map_rusqlite)?,
        last_summarized_at: last_summarized_at.as_deref().map(ts_from_db).transpose()?,
        created_at: ts_from_db(&row.get::<_, String>(9).map_err(map_rusqlite)?)?,
    })
}

#[async_trait]
impl ThreadRepository for SqliteThreadRepository {
    async fn insert(&self, thread: Thread) -> Result<(), StorageError> {
        let participant_domains = json_to_db(&thread.participant_domains)?;
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO threads (id, account_id, subject_root_normalized, \
                 participant_domains, message_count, first_seen_at, last_seen_at, \
                 last_summary, last_summarized_at, created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    thread.id.as_str(),
                    thread.account_id.as_str(),
                    thread.subject_root_normalized,
                    participant_domains,
                    thread.message_count,
                    ts_to_db(thread.first_seen_at),
                    ts_to_db(thread.last_seen_at),
                    thread.last_summary,
                    thread.last_summarized_at.map(ts_to_db),
                    ts_to_db(thread.created_at),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }

    async fn get(&self, id: &ThreadId) -> Result<Option<Thread>, StorageError> {
        let id = id.as_str().to_owned();
        self.backend.with_conn(|conn| {
            let sql = format!("SELECT {THREAD_COLUMNS} FROM threads WHERE id = ?1");
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![id]).map_err(map_rusqlite)?;
            match rows.next().map_err(map_rusqlite)? {
                Some(row) => Ok(Some(row_to_thread(row)?)),
                None => Ok(None),
            }
        })
    }

    async fn record_message_seen(
        &self,
        id: &ThreadId,
        seen_at: Timestamp,
    ) -> Result<(), StorageError> {
        let id = id.as_str().to_owned();
        let seen = ts_to_db(seen_at);
        self.backend.with_conn(|conn| {
            conn.execute(
                "UPDATE threads SET message_count = message_count + 1, last_seen_at = ?2 \
                 WHERE id = ?1",
                params![id, seen],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    use crate::backend::{open_and_migrate, StorageConfig};

    fn repo() -> SqliteThreadRepository {
        let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
        SqliteThreadRepository::new(backend)
    }

    fn sample(id: &ThreadId) -> Thread {
        Thread {
            id: id.clone(),
            account_id: AccountId::from("acct_a"),
            subject_root_normalized: "quote".to_owned(),
            participant_domains: vec!["example.com".to_owned()],
            message_count: 1,
            first_seen_at: Timestamp::now(),
            last_seen_at: Timestamp::now(),
            last_summary: None,
            last_summarized_at: None,
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn insert_get_round_trips_participant_domains() {
        let repo = repo();
        let id = ThreadId::fresh();
        block_on(repo.insert(sample(&id))).unwrap();
        let got = block_on(repo.get(&id)).unwrap().unwrap();
        assert_eq!(got.participant_domains, vec!["example.com".to_owned()]);
        assert_eq!(got.message_count, 1);
    }

    #[test]
    fn record_message_seen_bumps_the_counter() {
        let repo = repo();
        let id = ThreadId::fresh();
        block_on(repo.insert(sample(&id))).unwrap();
        block_on(repo.record_message_seen(&id, Timestamp::now())).unwrap();
        assert_eq!(block_on(repo.get(&id)).unwrap().unwrap().message_count, 2);
    }
}
