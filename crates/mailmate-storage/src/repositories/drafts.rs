//! SQLite-backed [`DraftRepository`].
//!
//! Enforces Drafting Safety at the storage boundary: every insert writes
//! `requires_review = 1` (a SQL literal, not a bound parameter), so no caller can persist
//! an auto-sendable draft.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Row};

use mailmate_common::draft::{DraftRecord, DraftStatus, NewDraft};
use mailmate_common::error::StorageError;
use mailmate_common::ids::{DraftId, MessageId, ThreadId};
use mailmate_common::time::Timestamp;
use mailmate_ports::storage::drafts::DraftRepository;

use crate::backend::{map_rusqlite, SqliteBackend};
use crate::repositories::convert::{json_from_db, json_to_db, ts_from_db, ts_to_db};

const DRAFT_COLUMNS: &str = "id, message_id, thread_id, provider_id, prompt_template_version, \
     subject, body, requires_review, safety_flags_json, status, created_at, updated_at";

/// SQLite implementation of [`DraftRepository`].
pub struct SqliteDraftRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteDraftRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

fn row_to_draft(row: &Row<'_>) -> Result<DraftRecord, StorageError> {
    let message_id: Option<String> = row.get(1).map_err(map_rusqlite)?;
    let thread_id: Option<String> = row.get(2).map_err(map_rusqlite)?;
    let requires_review: i64 = row.get(7).map_err(map_rusqlite)?;
    let safety_flags_json: String = row.get(8).map_err(map_rusqlite)?;
    let status_raw: String = row.get(9).map_err(map_rusqlite)?;
    let status = DraftStatus::from_db_str(&status_raw).ok_or_else(|| {
        StorageError::Serialization(format!("unknown draft status {status_raw:?}"))
    })?;
    let created_at: String = row.get(10).map_err(map_rusqlite)?;
    let updated_at: String = row.get(11).map_err(map_rusqlite)?;

    Ok(DraftRecord {
        id: DraftId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        message_id: message_id.map(MessageId::from),
        thread_id: thread_id.map(ThreadId::from),
        provider_id: row.get(3).map_err(map_rusqlite)?,
        prompt_template_version: row.get(4).map_err(map_rusqlite)?,
        subject: row.get(5).map_err(map_rusqlite)?,
        body: row.get(6).map_err(map_rusqlite)?,
        requires_review: requires_review != 0,
        safety_flags: json_from_db(&safety_flags_json)?,
        status,
        created_at: ts_from_db(&created_at)?,
        updated_at: ts_from_db(&updated_at)?,
    })
}

#[async_trait]
impl DraftRepository for SqliteDraftRepository {
    async fn insert(&self, draft: NewDraft) -> Result<DraftId, StorageError> {
        let safety_flags_json = json_to_db(&draft.safety_flags)?;
        let id = draft.id.clone();
        self.backend.with_conn(|conn| {
            conn.execute(
                // `requires_review` is the literal 1 — never caller-controlled.
                "INSERT INTO drafts (id, message_id, thread_id, provider_id, \
                 prompt_template_version, subject, body, requires_review, \
                 safety_flags_json, status, created_at, updated_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,1,?8,?9,?10,?11)",
                params![
                    draft.id.as_str(),
                    draft.message_id.as_ref().map(MessageId::as_str),
                    draft.thread_id.as_ref().map(ThreadId::as_str),
                    draft.provider_id,
                    draft.prompt_template_version,
                    draft.subject,
                    draft.body,
                    safety_flags_json,
                    DraftStatus::Generated.as_db_str(),
                    ts_to_db(draft.created_at),
                    ts_to_db(draft.updated_at),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn get(&self, id: &DraftId) -> Result<Option<DraftRecord>, StorageError> {
        let id = id.as_str().to_owned();
        self.backend.with_conn(|conn| {
            let sql = format!("SELECT {DRAFT_COLUMNS} FROM drafts WHERE id = ?1");
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![id]).map_err(map_rusqlite)?;
            match rows.next().map_err(map_rusqlite)? {
                Some(row) => Ok(Some(row_to_draft(row)?)),
                None => Ok(None),
            }
        })
    }

    async fn set_status(
        &self,
        id: &DraftId,
        status: DraftStatus,
        updated_at: Timestamp,
    ) -> Result<(), StorageError> {
        let id = id.as_str().to_owned();
        let updated = ts_to_db(updated_at);
        self.backend.with_conn(|conn| {
            conn.execute(
                "UPDATE drafts SET status = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, status.as_db_str(), updated],
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

    fn repo() -> SqliteDraftRepository {
        let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
        SqliteDraftRepository::new(backend)
    }

    fn sample(id: &DraftId) -> NewDraft {
        NewDraft {
            id: id.clone(),
            message_id: None,
            thread_id: None,
            provider_id: "mock".to_owned(),
            prompt_template_version: "v1".to_owned(),
            subject: "Re: Quote".to_owned(),
            body: "Thanks!".to_owned(),
            safety_flags: vec!["greeting_only".to_owned()],
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        }
    }

    #[test]
    fn insert_always_requires_review_and_starts_generated() {
        let repo = repo();
        let id = DraftId::fresh();
        block_on(repo.insert(sample(&id))).unwrap();
        let got = block_on(repo.get(&id)).unwrap().unwrap();
        assert!(got.requires_review, "drafts are always review-required");
        assert_eq!(got.status, DraftStatus::Generated);
        assert_eq!(got.safety_flags, vec!["greeting_only".to_owned()]);
    }

    #[test]
    fn set_status_advances_lifecycle() {
        let repo = repo();
        let id = DraftId::fresh();
        block_on(repo.insert(sample(&id))).unwrap();
        block_on(repo.set_status(&id, DraftStatus::Edited, Timestamp::now())).unwrap();
        assert_eq!(
            block_on(repo.get(&id)).unwrap().unwrap().status,
            DraftStatus::Edited
        );
    }
}
