//! SQLite-backed [`MessageRepository`].
//!
//! The privacy invariant lives here: [`insert`](SqliteMessageRepository) writes the
//! readable `body_text` only when the message's retention level retains bodies; otherwise
//! the column is `NULL` and `body_retained` is `0`. `body_hash` (identity/dedup) is kept
//! regardless — a hash is not the body.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Row};

use mailmate_common::error::StorageError;
use mailmate_common::features::FeatureVector;
use mailmate_common::ids::{AccountId, FolderId, MessageFeatureId, MessageId, ThreadId};
use mailmate_common::message::{ClassificationStatus, NewMessage, StoredFeature, StoredMessage};
use mailmate_common::time::Timestamp;
use mailmate_ports::storage::messages::MessageRepository;

use crate::backend::{map_rusqlite, SqliteBackend};
use crate::repositories::convert::{ts_from_db, ts_to_db};

const MESSAGE_COLUMNS: &str = "id, account_id, folder_id, thunderbird_message_id, \
     rfc_message_id_hash, thread_id, sender_email, sender_domain, subject, received_at, \
     classification_status, body_hash, body_retained, body_text, created_at";

/// SQLite implementation of [`MessageRepository`].
pub struct SqliteMessageRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteMessageRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

fn row_to_message(row: &Row<'_>) -> Result<StoredMessage, StorageError> {
    let status_raw: String = row.get(10).map_err(map_rusqlite)?;
    let status = ClassificationStatus::from_db_str(&status_raw).ok_or_else(|| {
        StorageError::Serialization(format!("unknown classification_status {status_raw:?}"))
    })?;
    let body_retained: i64 = row.get(12).map_err(map_rusqlite)?;
    let received_at: String = row.get(9).map_err(map_rusqlite)?;
    let created_at: String = row.get(14).map_err(map_rusqlite)?;
    let thread_id: Option<String> = row.get(5).map_err(map_rusqlite)?;

    Ok(StoredMessage {
        id: MessageId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        account_id: AccountId::from(row.get::<_, String>(1).map_err(map_rusqlite)?),
        folder_id: FolderId::from(row.get::<_, String>(2).map_err(map_rusqlite)?),
        thunderbird_message_id: row.get(3).map_err(map_rusqlite)?,
        rfc_message_id_hash: row.get(4).map_err(map_rusqlite)?,
        thread_id: thread_id.map(ThreadId::from),
        sender_email: row.get(6).map_err(map_rusqlite)?,
        sender_domain: row.get(7).map_err(map_rusqlite)?,
        subject: row.get(8).map_err(map_rusqlite)?,
        received_at: ts_from_db(&received_at)?,
        classification_status: status,
        body_hash: row.get(11).map_err(map_rusqlite)?,
        body_retained: body_retained != 0,
        body_text: row.get(13).map_err(map_rusqlite)?,
        created_at: ts_from_db(&created_at)?,
    })
}

#[async_trait]
impl MessageRepository for SqliteMessageRepository {
    async fn insert(&self, message: NewMessage) -> Result<MessageId, StorageError> {
        // The privacy gate: keep the readable body ONLY if retention allows it.
        let retains_body = message.retention.retains_body();
        let body_to_store = if retains_body {
            message.body_text.clone()
        } else {
            None
        };
        let id = message.id.clone();
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO messages (id, account_id, folder_id, thunderbird_message_id, \
                 rfc_message_id_hash, thread_id, sender_email, sender_domain, subject, \
                 received_at, classification_status, body_hash, body_retained, body_text, \
                 created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![
                    message.id.as_str(),
                    message.account_id.as_str(),
                    message.folder_id.as_str(),
                    message.thunderbird_message_id,
                    message.rfc_message_id_hash,
                    message.thread_id.as_ref().map(ThreadId::as_str),
                    message.sender_email,
                    message.sender_domain,
                    message.subject,
                    ts_to_db(message.received_at),
                    ClassificationStatus::Pending.as_db_str(),
                    message.body_hash,
                    i64::from(retains_body),
                    body_to_store,
                    ts_to_db(message.created_at),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn get(&self, id: &MessageId) -> Result<Option<StoredMessage>, StorageError> {
        let id = id.as_str().to_owned();
        self.backend.with_conn(|conn| {
            let sql = format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE id = ?1");
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![id]).map_err(map_rusqlite)?;
            match rows.next().map_err(map_rusqlite)? {
                Some(row) => Ok(Some(row_to_message(row)?)),
                None => Ok(None),
            }
        })
    }

    async fn set_classification_status(
        &self,
        id: &MessageId,
        status: ClassificationStatus,
    ) -> Result<(), StorageError> {
        let id = id.as_str().to_owned();
        self.backend.with_conn(|conn| {
            conn.execute(
                "UPDATE messages SET classification_status = ?2 WHERE id = ?1",
                params![id, status.as_db_str()],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }

    async fn list_by_status(
        &self,
        status: ClassificationStatus,
    ) -> Result<Vec<StoredMessage>, StorageError> {
        self.backend.with_conn(|conn| {
            let sql = format!(
                "SELECT {MESSAGE_COLUMNS} FROM messages WHERE classification_status = ?1 \
                 ORDER BY received_at, id"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt
                .query(params![status.as_db_str()])
                .map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_message(row)?);
            }
            Ok(out)
        })
    }

    async fn add_features(
        &self,
        message_id: &MessageId,
        features: &FeatureVector,
    ) -> Result<(), StorageError> {
        // Serialize each value to its JSON scalar/object text outside the lock.
        let rows: Vec<(String, String)> = features
            .features
            .iter()
            .map(|(name, value)| {
                let serialized = serde_json::to_string(value)
                    .map_err(|e| StorageError::Serialization(e.to_string()))?;
                Ok::<_, StorageError>((name.clone(), serialized))
            })
            .collect::<Result<_, _>>()?;
        let message_id = message_id.as_str().to_owned();
        let created = ts_to_db(Timestamp::now());
        self.backend.with_conn(|conn| {
            for (name, value) in &rows {
                let feature_id = MessageFeatureId::fresh();
                conn.execute(
                    "INSERT INTO message_features (id, message_id, feature_name, \
                     feature_value, created_at) VALUES (?1,?2,?3,?4,?5)",
                    params![feature_id.as_str(), message_id, name, value, created],
                )
                .map_err(map_rusqlite)?;
            }
            Ok(())
        })
    }

    async fn get_features(
        &self,
        message_id: &MessageId,
    ) -> Result<Vec<StoredFeature>, StorageError> {
        let message_id = message_id.as_str().to_owned();
        self.backend.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, message_id, feature_name, feature_value, created_at \
                     FROM message_features WHERE message_id = ?1 ORDER BY feature_name",
                )
                .map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![message_id]).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                let created: String = row.get(4).map_err(map_rusqlite)?;
                out.push(StoredFeature {
                    id: MessageFeatureId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
                    message_id: MessageId::from(row.get::<_, String>(1).map_err(map_rusqlite)?),
                    feature_name: row.get(2).map_err(map_rusqlite)?,
                    feature_value: row.get(3).map_err(map_rusqlite)?,
                    created_at: ts_from_db(&created)?,
                });
            }
            Ok(out)
        })
    }

    async fn purge_bodies(&self) -> Result<u64, StorageError> {
        self.backend.with_conn(|conn| {
            // NULL the readable body and clear the retained flag wherever a body is present;
            // body_hash (identity/dedup) is deliberately left intact.
            let purged = conn
                .execute(
                    "UPDATE messages SET body_text = NULL, body_retained = 0 \
                     WHERE body_text IS NOT NULL",
                    [],
                )
                .map_err(map_rusqlite)?;
            Ok(purged as u64)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    use mailmate_common::retention::RetentionLevel;
    use rusqlite::OptionalExtension;

    use crate::backend::{open_and_migrate, StorageConfig};

    fn new_message(id: &MessageId, body: Option<&str>, retention: RetentionLevel) -> NewMessage {
        NewMessage {
            id: id.clone(),
            account_id: AccountId::from("acct_a"),
            folder_id: FolderId::from("folder_inbox"),
            thunderbird_message_id: "42".to_owned(),
            rfc_message_id_hash: Some("rfchash".to_owned()),
            thread_id: None,
            sender_email: "s@example.com".to_owned(),
            sender_domain: "example.com".to_owned(),
            subject: "Quote".to_owned(),
            received_at: Timestamp::now(),
            body_hash: Some("bodyhash".to_owned()),
            body_text: body.map(str::to_owned),
            retention,
            created_at: Timestamp::now(),
        }
    }

    fn repo() -> SqliteMessageRepository {
        let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
        SqliteMessageRepository::new(backend)
    }

    #[test]
    fn metadata_retention_never_writes_the_body_to_disk() {
        let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
        let repo = SqliteMessageRepository::new(Arc::clone(&backend));
        let id = MessageId::fresh();
        block_on(repo.insert(new_message(
            &id,
            Some("TOP SECRET BODY"),
            RetentionLevel::Metadata,
        )))
        .unwrap();

        // Raw-column proof: the body_text column is NULL — the body never touched disk —
        // while body_hash (identity/dedup) is still present.
        let (body_text, body_retained, body_hash): (Option<String>, i64, Option<String>) = backend
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT body_text, body_retained, body_hash FROM messages WHERE id = ?1",
                    params![id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(map_rusqlite)?
                .ok_or_else(|| StorageError::Backend("row missing".to_owned()))
            })
            .unwrap();
        assert_eq!(
            body_text, None,
            "default retention must not persist the body"
        );
        assert_eq!(body_retained, 0);
        assert_eq!(
            body_hash,
            Some("bodyhash".to_owned()),
            "hash is kept for dedup"
        );
    }

    #[test]
    fn opted_in_body_retention_stores_the_body() {
        let repo = repo();
        let id = MessageId::fresh();
        block_on(repo.insert(new_message(&id, Some("kept body"), RetentionLevel::Bodies))).unwrap();
        let stored = block_on(repo.get(&id)).unwrap().unwrap();
        assert!(stored.body_retained);
        assert_eq!(stored.body_text.as_deref(), Some("kept body"));
    }

    #[test]
    fn purge_bodies_clears_retained_bodies_but_keeps_the_hash() {
        // The down-level purge: after opting in and storing a body, lowering retention purges it
        // — body_text NULL, body_retained 0 — while body_hash (identity) survives.
        let repo = repo();
        let kept = MessageId::fresh();
        block_on(repo.insert(new_message(
            &kept,
            Some("opted-in body"),
            RetentionLevel::Bodies,
        )))
        .unwrap();
        let meta = MessageId::fresh();
        block_on(repo.insert(new_message(
            &meta,
            Some("never stored"),
            RetentionLevel::Metadata,
        )))
        .unwrap();

        // Only the one retained body is purged.
        let purged = block_on(repo.purge_bodies()).unwrap();
        assert_eq!(purged, 1, "exactly the one retained body is purged");

        let after = block_on(repo.get(&kept)).unwrap().unwrap();
        assert_eq!(after.body_text, None, "the body is gone after the purge");
        assert!(!after.body_retained);
        assert_eq!(
            after.body_hash.as_deref(),
            Some("bodyhash"),
            "identity hash survives the purge"
        );

        // A second purge is a no-op (idempotent).
        assert_eq!(block_on(repo.purge_bodies()).unwrap(), 0);
    }

    #[test]
    fn insert_then_get_round_trips_and_defaults_to_pending() {
        let repo = repo();
        let id = MessageId::fresh();
        block_on(repo.insert(new_message(&id, None, RetentionLevel::Metadata))).unwrap();
        let stored = block_on(repo.get(&id)).unwrap().unwrap();
        assert_eq!(stored.classification_status, ClassificationStatus::Pending);
        assert_eq!(stored.sender_domain, "example.com");
        assert!(block_on(repo.get(&MessageId::fresh())).unwrap().is_none());
    }
}
