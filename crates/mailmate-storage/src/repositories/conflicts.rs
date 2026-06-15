//! SQLite-backed [`ConflictRepository`] — the recorded rule-conflict store.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Row};

use mailmate_common::conflict::{ConflictStatus, RuleConflictRecord};
use mailmate_common::error::StorageError;
use mailmate_common::ids::{ConflictId, RuleId};
use mailmate_common::rules::evaluation::{ConflictKind, ConflictSeverity};
use mailmate_common::rules::rule::RuleKind;
use mailmate_ports::storage::conflicts::ConflictRepository;

use crate::backend::{map_rusqlite, SqliteBackend};
use crate::repositories::convert::{ts_from_db, ts_to_db};

const CONFLICT_COLUMNS: &str = "id, rule_kind, rule_a_id, rule_b_id, conflict_kind, severity, \
     description, status, created_at, resolved_at";

/// SQLite implementation of [`ConflictRepository`].
pub struct SqliteConflictRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteConflictRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

fn row_to_conflict(row: &Row<'_>) -> Result<RuleConflictRecord, StorageError> {
    let kind_raw: String = row.get(1).map_err(map_rusqlite)?;
    let conflict_kind_raw: String = row.get(4).map_err(map_rusqlite)?;
    let severity_raw: String = row.get(5).map_err(map_rusqlite)?;
    let status_raw: String = row.get(7).map_err(map_rusqlite)?;
    let created_at: String = row.get(8).map_err(map_rusqlite)?;
    let resolved_at: Option<String> = row.get(9).map_err(map_rusqlite)?;
    Ok(RuleConflictRecord {
        id: ConflictId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        rule_kind: RuleKind::from_db_str(&kind_raw).ok_or_else(|| {
            StorageError::Serialization(format!("unknown rule kind {kind_raw:?}"))
        })?,
        rule_a_id: RuleId::from(row.get::<_, String>(2).map_err(map_rusqlite)?),
        rule_b_id: RuleId::from(row.get::<_, String>(3).map_err(map_rusqlite)?),
        conflict_kind: ConflictKind::from_db_str(&conflict_kind_raw).ok_or_else(|| {
            StorageError::Serialization(format!("unknown conflict kind {conflict_kind_raw:?}"))
        })?,
        severity: ConflictSeverity::from_db_str(&severity_raw).ok_or_else(|| {
            StorageError::Serialization(format!("unknown conflict severity {severity_raw:?}"))
        })?,
        description: row.get(6).map_err(map_rusqlite)?,
        status: ConflictStatus::from_db_str(&status_raw).ok_or_else(|| {
            StorageError::Serialization(format!("unknown conflict status {status_raw:?}"))
        })?,
        created_at: ts_from_db(&created_at)?,
        resolved_at: match resolved_at {
            Some(s) => Some(ts_from_db(&s)?),
            None => None,
        },
    })
}

#[async_trait]
impl ConflictRepository for SqliteConflictRepository {
    async fn append(&self, conflict: RuleConflictRecord) -> Result<ConflictId, StorageError> {
        let id = conflict.id.clone();
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO rule_conflicts (id, rule_kind, rule_a_id, rule_b_id, conflict_kind, \
                 severity, description, status, created_at, resolved_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    conflict.id.as_str(),
                    conflict.rule_kind.as_str(),
                    conflict.rule_a_id.as_str(),
                    conflict.rule_b_id.as_str(),
                    conflict.conflict_kind.as_str(),
                    conflict.severity.as_str(),
                    conflict.description,
                    conflict.status.as_str(),
                    ts_to_db(conflict.created_at),
                    conflict.resolved_at.map(ts_to_db),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn list_open(&self) -> Result<Vec<RuleConflictRecord>, StorageError> {
        self.backend.with_conn(|conn| {
            let sql = format!(
                "SELECT {CONFLICT_COLUMNS} FROM rule_conflicts WHERE status = ?1 \
                 ORDER BY created_at DESC, id DESC"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt
                .query(params![ConflictStatus::Open.as_str()])
                .map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_conflict(row)?);
            }
            Ok(out)
        })
    }

    async fn set_status(
        &self,
        id: &ConflictId,
        status: ConflictStatus,
        resolved_at: Option<mailmate_common::time::Timestamp>,
    ) -> Result<(), StorageError> {
        let id = id.as_str().to_owned();
        let resolved = resolved_at.map(ts_to_db);
        self.backend.with_conn(|conn| {
            conn.execute(
                "UPDATE rule_conflicts SET status = ?2, resolved_at = ?3 WHERE id = ?1",
                params![id, status.as_str(), resolved],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }
}
