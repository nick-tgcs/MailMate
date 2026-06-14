//! SQLite-backed [`ShadowOutcomeRepository`] — the live-shadow-firing record.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Row};

use mailmate_common::error::StorageError;
use mailmate_common::ids::{MessageId, RuleId, RuleVersionId, ShadowOutcomeId};
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::rule::RuleKind;
use mailmate_common::shadow::ShadowOutcomeRow;
use mailmate_ports::storage::shadow_outcomes::ShadowOutcomeRepository;

use crate::backend::{map_rusqlite, SqliteBackend};
use crate::repositories::convert::{json_from_db, json_to_db, ts_from_db, ts_to_db};

const SHADOW_COLUMNS: &str = "id, rule_kind, rule_id, rule_version_id, message_id, \
     would_have_action_json, would_have_policy_outcome, matched_later_user_action, created_at";

/// SQLite implementation of [`ShadowOutcomeRepository`].
pub struct SqliteShadowOutcomeRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteShadowOutcomeRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

fn row_to_shadow(row: &Row<'_>) -> Result<ShadowOutcomeRow, StorageError> {
    let kind_raw: String = row.get(1).map_err(map_rusqlite)?;
    let action_raw: String = row.get(5).map_err(map_rusqlite)?;
    let matched: Option<i64> = row.get(7).map_err(map_rusqlite)?;
    let created_at: String = row.get(8).map_err(map_rusqlite)?;
    Ok(ShadowOutcomeRow {
        id: ShadowOutcomeId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        rule_kind: RuleKind::from_db_str(&kind_raw).ok_or_else(|| {
            StorageError::Serialization(format!("unknown rule kind {kind_raw:?}"))
        })?,
        rule_id: RuleId::from(row.get::<_, String>(2).map_err(map_rusqlite)?),
        rule_version_id: RuleVersionId::from(row.get::<_, String>(3).map_err(map_rusqlite)?),
        message_id: MessageId::from(row.get::<_, String>(4).map_err(map_rusqlite)?),
        would_have_action: json_from_db::<RuleEffect>(&action_raw)?,
        would_have_policy_outcome: row.get(6).map_err(map_rusqlite)?,
        matched_later_user_action: matched.map(|v| v != 0),
        created_at: ts_from_db(&created_at)?,
    })
}

#[async_trait]
impl ShadowOutcomeRepository for SqliteShadowOutcomeRepository {
    async fn append(&self, row: ShadowOutcomeRow) -> Result<ShadowOutcomeId, StorageError> {
        let action_json = json_to_db(&row.would_have_action)?;
        let id = row.id.clone();
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO shadow_outcomes (id, rule_kind, rule_id, rule_version_id, message_id, \
                 would_have_action_json, would_have_policy_outcome, matched_later_user_action, \
                 created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    row.id.as_str(),
                    row.rule_kind.as_str(),
                    row.rule_id.as_str(),
                    row.rule_version_id.as_str(),
                    row.message_id.as_str(),
                    action_json,
                    row.would_have_policy_outcome,
                    row.matched_later_user_action.map(i64::from),
                    ts_to_db(row.created_at),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn list_for_rule(&self, rule_id: &RuleId) -> Result<Vec<ShadowOutcomeRow>, StorageError> {
        let rule_id = rule_id.as_str().to_owned();
        self.backend.with_conn(|conn| {
            let sql = format!(
                "SELECT {SHADOW_COLUMNS} FROM shadow_outcomes WHERE rule_id = ?1 \
                 ORDER BY created_at, id"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![rule_id]).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_shadow(row)?);
            }
            Ok(out)
        })
    }
}
