//! SQLite-backed [`PipelineItemRepository`] — the sales-pipeline item store. A tracker, not
//! a CRM: a row is a stage, a thread anchor, a counterparty, and a display-only amount hint.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Row};

use mailmate_common::actor::Actor;
use mailmate_common::error::StorageError;
use mailmate_common::ids::{MessageId, PipelineItemId, ThreadId};
use mailmate_common::pipeline::{
    ItemType, NewPipelineItem, PipelineItem, PipelineItemQuery, PipelineStage,
};
use mailmate_common::time::Timestamp;
use mailmate_ports::storage::pipeline_items::PipelineItemRepository;

use crate::backend::{map_rusqlite, SqliteBackend};
use crate::repositories::convert::{ts_from_db, ts_to_db};

/// SQLite implementation of the pipeline-item repository.
pub struct SqlitePipelineItemRepository {
    backend: Arc<SqliteBackend>,
}

impl SqlitePipelineItemRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

const PIPELINE_COLUMNS: &str = "id, account_id, thread_id, anchor_message_id, counterparty_email, \
     counterparty_domain, title, item_type, stage, amount_hint, last_activity_at, created_by, \
     created_at, updated_at";

fn item_type_from_db(raw: &str) -> Result<ItemType, StorageError> {
    ItemType::from_db_str(raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown item type {raw:?}")))
}

fn stage_from_db(raw: &str) -> Result<PipelineStage, StorageError> {
    PipelineStage::from_db_str(raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown pipeline stage {raw:?}")))
}

fn actor_from_db(raw: &str) -> Result<Actor, StorageError> {
    Actor::from_db_str(raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown actor {raw:?}")))
}

fn row_to_item(row: &Row<'_>) -> Result<PipelineItem, StorageError> {
    let anchor_raw: Option<String> = row.get(3).map_err(map_rusqlite)?;
    let item_type_raw: String = row.get(7).map_err(map_rusqlite)?;
    let stage_raw: String = row.get(8).map_err(map_rusqlite)?;
    let last_activity: String = row.get(10).map_err(map_rusqlite)?;
    let created_by_raw: String = row.get(11).map_err(map_rusqlite)?;
    let created_at: String = row.get(12).map_err(map_rusqlite)?;
    let updated_at: String = row.get(13).map_err(map_rusqlite)?;
    Ok(PipelineItem {
        id: PipelineItemId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        account_id: row.get(1).map_err(map_rusqlite)?,
        thread_id: ThreadId::from(row.get::<_, String>(2).map_err(map_rusqlite)?),
        anchor_message_id: anchor_raw.map(MessageId::from),
        counterparty_email: row.get(4).map_err(map_rusqlite)?,
        counterparty_domain: row.get(5).map_err(map_rusqlite)?,
        title: row.get(6).map_err(map_rusqlite)?,
        item_type: item_type_from_db(&item_type_raw)?,
        stage: stage_from_db(&stage_raw)?,
        amount_hint: row.get(9).map_err(map_rusqlite)?,
        last_activity_at: ts_from_db(&last_activity)?,
        created_by: actor_from_db(&created_by_raw)?,
        created_at: ts_from_db(&created_at)?,
        updated_at: ts_from_db(&updated_at)?,
    })
}

#[async_trait]
impl PipelineItemRepository for SqlitePipelineItemRepository {
    async fn insert(&self, item: NewPipelineItem) -> Result<PipelineItemId, StorageError> {
        let id = PipelineItemId::fresh();
        let now = ts_to_db(Timestamp::now());
        let result = id.clone();
        // An item is always user-enrolled (the AI may suggest, but never enrolls) and starts
        // in the `open` stage — the repository pins both so a caller cannot do otherwise.
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO pipeline_items (id, account_id, thread_id, anchor_message_id, \
                 counterparty_email, counterparty_domain, title, item_type, stage, amount_hint, \
                 last_activity_at, created_by, created_at, updated_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?13)",
                params![
                    id.as_str(),
                    item.account_id,
                    item.thread_id.as_str(),
                    item.anchor_message_id.as_ref().map(MessageId::as_str),
                    item.counterparty_email,
                    item.counterparty_domain,
                    item.title,
                    item.item_type.as_str(),
                    PipelineStage::Open.as_str(),
                    item.amount_hint,
                    now,
                    Actor::User.as_str(),
                    now,
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(result)
    }

    async fn get(&self, id: &PipelineItemId) -> Result<Option<PipelineItem>, StorageError> {
        self.backend.with_conn(|conn| {
            let sql = format!("SELECT {PIPELINE_COLUMNS} FROM pipeline_items WHERE id = ?1");
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![id.as_str()]).map_err(map_rusqlite)?;
            match rows.next().map_err(map_rusqlite)? {
                Some(row) => Ok(Some(row_to_item(row)?)),
                None => Ok(None),
            }
        })
    }

    async fn get_by_thread(&self, thread_id: &ThreadId) -> Result<Vec<PipelineItem>, StorageError> {
        self.backend.with_conn(|conn| {
            let sql = format!(
                "SELECT {PIPELINE_COLUMNS} FROM pipeline_items WHERE thread_id = ?1 \
                 ORDER BY created_at DESC, id DESC"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt
                .query(params![thread_id.as_str()])
                .map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_item(row)?);
            }
            Ok(out)
        })
    }

    async fn update_stage(
        &self,
        id: &PipelineItemId,
        stage: PipelineStage,
    ) -> Result<(), StorageError> {
        let now = ts_to_db(Timestamp::now());
        self.backend.with_conn(|conn| {
            conn.execute(
                "UPDATE pipeline_items SET stage = ?2, updated_at = ?3, last_activity_at = ?3 \
                 WHERE id = ?1",
                params![id.as_str(), stage.as_str(), now],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }

    async fn query(&self, query: PipelineItemQuery) -> Result<Vec<PipelineItem>, StorageError> {
        self.backend.with_conn(|conn| {
            let mut binds: Vec<String> = Vec::new();
            let mut predicates: Vec<String> = Vec::new();
            if let Some(account_id) = &query.account_id {
                binds.push(account_id.clone());
                predicates.push(format!("account_id = ?{}", binds.len()));
            }
            if let Some(stage) = query.stage {
                binds.push(stage.as_str().to_owned());
                predicates.push(format!("stage = ?{}", binds.len()));
            }
            let where_sql = if predicates.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", predicates.join(" AND "))
            };
            let sql = format!(
                "SELECT {PIPELINE_COLUMNS} FROM pipeline_items {where_sql} \
                 ORDER BY last_activity_at DESC, id DESC {}",
                match query.limit {
                    Some(n) => format!("LIMIT {n}"),
                    None => String::new(),
                }
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let refs: Vec<&dyn rusqlite::ToSql> =
                binds.iter().map(|b| b as &dyn rusqlite::ToSql).collect();
            let mut rows = stmt.query(refs.as_slice()).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_item(row)?);
            }
            Ok(out)
        })
    }
}
