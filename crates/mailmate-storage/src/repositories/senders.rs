//! SQLite-backed [`SenderRepository`].

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Row};

use mailmate_common::error::StorageError;
use mailmate_common::ids::SenderId;
use mailmate_common::sender::{SenderProfile, TrustLevel};
use mailmate_common::time::Timestamp;
use mailmate_ports::storage::senders::SenderRepository;

use crate::backend::{map_rusqlite, SqliteBackend};
use crate::repositories::convert::{json_from_db, json_to_db, ts_from_db, ts_to_db};

const SENDER_COLUMNS: &str = "id, email, domain, display_name, trust_level, last_seen_at, \
     feature_json, created_at, updated_at";

/// SQLite implementation of [`SenderRepository`].
pub struct SqliteSenderRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteSenderRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

fn row_to_sender(row: &Row<'_>) -> Result<SenderProfile, StorageError> {
    let trust_raw: String = row.get(4).map_err(map_rusqlite)?;
    let trust_level = TrustLevel::from_db_str(&trust_raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown trust_level {trust_raw:?}")))?;
    let last_seen_at: String = row.get(5).map_err(map_rusqlite)?;
    let feature_json: String = row.get(6).map_err(map_rusqlite)?;
    let created_at: String = row.get(7).map_err(map_rusqlite)?;
    let updated_at: String = row.get(8).map_err(map_rusqlite)?;

    Ok(SenderProfile {
        id: SenderId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        email: row.get(1).map_err(map_rusqlite)?,
        domain: row.get(2).map_err(map_rusqlite)?,
        display_name: row.get(3).map_err(map_rusqlite)?,
        trust_level,
        last_seen_at: ts_from_db(&last_seen_at)?,
        feature_json: json_from_db(&feature_json)?,
        created_at: ts_from_db(&created_at)?,
        updated_at: ts_from_db(&updated_at)?,
    })
}

#[async_trait]
impl SenderRepository for SqliteSenderRepository {
    async fn insert(&self, profile: SenderProfile) -> Result<(), StorageError> {
        let feature_json = json_to_db(&profile.feature_json)?;
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO sender_profiles (id, email, domain, display_name, trust_level, \
                 last_seen_at, feature_json, created_at, updated_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    profile.id.as_str(),
                    profile.email,
                    profile.domain,
                    profile.display_name,
                    profile.trust_level.as_db_str(),
                    ts_to_db(profile.last_seen_at),
                    feature_json,
                    ts_to_db(profile.created_at),
                    ts_to_db(profile.updated_at),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }

    async fn get_by_email(&self, email: &str) -> Result<Option<SenderProfile>, StorageError> {
        let email = email.to_owned();
        self.backend.with_conn(|conn| {
            let sql = format!(
                "SELECT {SENDER_COLUMNS} FROM sender_profiles WHERE email = ?1 \
                 ORDER BY updated_at DESC, id LIMIT 1"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![email]).map_err(map_rusqlite)?;
            match rows.next().map_err(map_rusqlite)? {
                Some(row) => Ok(Some(row_to_sender(row)?)),
                None => Ok(None),
            }
        })
    }

    async fn set_trust_level(
        &self,
        id: &SenderId,
        trust: TrustLevel,
        updated_at: Timestamp,
    ) -> Result<(), StorageError> {
        let id = id.as_str().to_owned();
        let updated = ts_to_db(updated_at);
        self.backend.with_conn(|conn| {
            conn.execute(
                "UPDATE sender_profiles SET trust_level = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, trust.as_db_str(), updated],
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
    use serde_json::json;

    use crate::backend::{open_and_migrate, StorageConfig};

    fn repo() -> SqliteSenderRepository {
        let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
        SqliteSenderRepository::new(backend)
    }

    fn sample(id: &SenderId) -> SenderProfile {
        SenderProfile {
            id: id.clone(),
            email: "s@example.com".to_owned(),
            domain: "example.com".to_owned(),
            display_name: Some("Sam".to_owned()),
            trust_level: TrustLevel::Unknown,
            last_seen_at: Timestamp::now(),
            feature_json: json!({ "seen": 1 }),
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        }
    }

    #[test]
    fn insert_get_round_trips_and_updates_trust() {
        let repo = repo();
        let id = SenderId::fresh();
        block_on(repo.insert(sample(&id))).unwrap();
        let got = block_on(repo.get_by_email("s@example.com"))
            .unwrap()
            .unwrap();
        assert_eq!(got.trust_level, TrustLevel::Unknown);
        assert_eq!(got.feature_json, json!({ "seen": 1 }));

        block_on(repo.set_trust_level(&id, TrustLevel::Trusted, Timestamp::now())).unwrap();
        let updated = block_on(repo.get_by_email("s@example.com"))
            .unwrap()
            .unwrap();
        assert_eq!(updated.trust_level, TrustLevel::Trusted);

        assert!(block_on(repo.get_by_email("nobody@example.com"))
            .unwrap()
            .is_none());
    }
}
