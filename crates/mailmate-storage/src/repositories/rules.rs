//! SQLite-backed [`RuleRepository`].
//!
//! Two pipelines, two table pairs, one mechanism: every method selects its tables from the
//! [`RuleKind`] and runs the same SQL shape. A draft is written as a rule row plus its
//! first immutable version inside one transaction (insert rule, insert version, repoint
//! `current_version_id`), so the circular rule↔version reference is never observable
//! half-written. Reads reconstruct an [`EvaluatableRule`] by joining a rule to its current
//! version.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Row};

use mailmate_common::error::StorageError;
use mailmate_common::ids::{RuleId, RuleVersionId};
use mailmate_common::rules::condition::Condition;
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::rule::{
    EvaluatableRule, HierarchyBand, NewRule, NewRuleVersion, RiskLevel, RuleKind, RuleScope,
    RuleStatus, RuleVersion, RuleVersionContent,
};
use mailmate_common::time::Timestamp;
use mailmate_ports::storage::rules::RuleRepository;

use crate::backend::{map_rusqlite, SqliteBackend};
use crate::repositories::convert::{json_from_db, json_to_db, ts_to_db};

/// The rule + version table names for a [`RuleKind`].
fn tables(kind: RuleKind) -> (&'static str, &'static str) {
    match kind {
        RuleKind::Classification => ("classification_rules", "classification_rule_versions"),
        RuleKind::Action => ("action_rules", "action_rule_versions"),
    }
}

/// SQLite implementation of [`RuleRepository`].
pub struct SqliteRuleRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteRuleRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }

    /// Load the `status`+`scope` snapshot of `kind`, joining each rule to its current
    /// version.
    fn snapshot(
        &self,
        kind: RuleKind,
        scope: RuleScope,
        status: RuleStatus,
    ) -> Result<Vec<EvaluatableRule>, StorageError> {
        let (rules, versions) = tables(kind);
        let sql = format!(
            "SELECT r.id, r.scope, r.band, r.status, \
                    v.id, v.version_number, v.condition_json, v.effect_json, v.risk_level \
             FROM {rules} r JOIN {versions} v ON v.id = r.current_version_id \
             WHERE r.status = ?1 AND r.scope = ?2 ORDER BY r.created_at, r.id"
        );
        self.backend.with_conn(|conn| {
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt
                .query(params![status.as_str(), scope.as_str()])
                .map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_evaluatable(kind, row)?);
            }
            Ok(out)
        })
    }
}

fn row_to_evaluatable(kind: RuleKind, row: &Row<'_>) -> Result<EvaluatableRule, StorageError> {
    let scope_raw: String = row.get(1).map_err(map_rusqlite)?;
    let band_raw: String = row.get(2).map_err(map_rusqlite)?;
    let status_raw: String = row.get(3).map_err(map_rusqlite)?;
    let condition_raw: String = row.get(6).map_err(map_rusqlite)?;
    let effect_raw: String = row.get(7).map_err(map_rusqlite)?;
    let risk_raw: String = row.get(8).map_err(map_rusqlite)?;

    let scope = RuleScope::from_db_str(&scope_raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown rule scope {scope_raw:?}")))?;
    let band = HierarchyBand::from_db_str(&band_raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown rule band {band_raw:?}")))?;
    let status = RuleStatus::from_db_str(&status_raw).ok_or_else(|| {
        StorageError::Serialization(format!("unknown rule status {status_raw:?}"))
    })?;
    let risk_level = RiskLevel::from_db_str(&risk_raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown risk level {risk_raw:?}")))?;
    let condition: Condition = json_from_db(&condition_raw)?;
    let effect: RuleEffect = json_from_db(&effect_raw)?;

    Ok(EvaluatableRule {
        rule_id: RuleId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        kind,
        scope,
        band,
        status,
        version: RuleVersion {
            id: RuleVersionId::from(row.get::<_, String>(4).map_err(map_rusqlite)?),
            version_number: row.get(5).map_err(map_rusqlite)?,
            condition,
            effect,
            risk_level,
        },
    })
}

/// Insert one version row; the caller supplies the assigned id and number. Runs on a
/// connection already inside a transaction.
fn insert_version(
    conn: &rusqlite::Connection,
    versions_table: &str,
    version_id: &RuleVersionId,
    rule_id: &RuleId,
    version_number: i64,
    content: &RuleVersionContent,
) -> Result<(), StorageError> {
    let condition_json = json_to_db(&content.condition)?;
    let effect_json = json_to_db(&content.effect)?;
    let sql = format!(
        "INSERT INTO {versions_table} (id, rule_id, version_number, title, description, \
         condition_json, effect_json, priority, confidence_threshold, risk_level, created_by, \
         change_reason, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)"
    );
    conn.execute(
        &sql,
        params![
            version_id.as_str(),
            rule_id.as_str(),
            version_number,
            content.title,
            content.description,
            condition_json,
            effect_json,
            content.priority,
            content.confidence_threshold,
            content.risk_level.as_str(),
            content.created_by.as_str(),
            content.change_reason,
            ts_to_db(Timestamp::now()),
        ],
    )
    .map_err(map_rusqlite)?;
    Ok(())
}

#[async_trait]
impl RuleRepository for SqliteRuleRepository {
    async fn get_active_rules(
        &self,
        kind: RuleKind,
        scope: RuleScope,
    ) -> Result<Vec<EvaluatableRule>, StorageError> {
        self.snapshot(kind, scope, RuleStatus::Active)
    }

    async fn get_shadow_rules(
        &self,
        kind: RuleKind,
        scope: RuleScope,
    ) -> Result<Vec<EvaluatableRule>, StorageError> {
        self.snapshot(kind, scope, RuleStatus::ShadowMode)
    }

    async fn save_rule_draft(&self, draft: NewRule) -> Result<RuleId, StorageError> {
        let (rules, versions) = tables(draft.kind);
        let rule_id = RuleId::fresh();
        let version_id = RuleVersionId::fresh();
        let now = ts_to_db(Timestamp::now());
        let result_id = rule_id.clone();
        self.backend.with_conn_mut(|conn| {
            let tx = conn.transaction().map_err(map_rusqlite)?;
            let insert_rule = format!(
                "INSERT INTO {rules} (id, stable_name, scope, band, status, current_version_id, \
                 created_by, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,NULL,?6,?7,?7)"
            );
            tx.execute(
                &insert_rule,
                params![
                    rule_id.as_str(),
                    draft.stable_name,
                    draft.scope.as_str(),
                    draft.band.as_str(),
                    RuleStatus::Draft.as_str(),
                    draft.created_by.as_str(),
                    now,
                ],
            )
            .map_err(map_rusqlite)?;
            insert_version(
                &tx,
                versions,
                &version_id,
                &rule_id,
                1,
                &draft.initial_version,
            )?;
            let repoint = format!("UPDATE {rules} SET current_version_id = ?2 WHERE id = ?1");
            tx.execute(&repoint, params![rule_id.as_str(), version_id.as_str()])
                .map_err(map_rusqlite)?;
            tx.commit().map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(result_id)
    }

    async fn create_rule_version(
        &self,
        version: NewRuleVersion,
    ) -> Result<RuleVersionId, StorageError> {
        let (rules, versions) = tables(version.kind);
        let version_id = RuleVersionId::fresh();
        let now = ts_to_db(Timestamp::now());
        let result_id = version_id.clone();
        self.backend.with_conn_mut(|conn| {
            let tx = conn.transaction().map_err(map_rusqlite)?;
            // The next monotonic version number for this rule (0 if somehow none yet).
            let max_sql = format!(
                "SELECT COALESCE(MAX(version_number), 0) FROM {versions} WHERE rule_id = ?1"
            );
            let next: i64 = tx
                .query_row(&max_sql, params![version.rule_id.as_str()], |row| {
                    row.get(0)
                })
                .map_err(map_rusqlite)?;
            insert_version(
                &tx,
                versions,
                &version_id,
                &version.rule_id,
                next + 1,
                &version.content,
            )?;
            let repoint = format!(
                "UPDATE {rules} SET current_version_id = ?2, updated_at = ?3 WHERE id = ?1"
            );
            tx.execute(
                &repoint,
                params![version.rule_id.as_str(), version_id.as_str(), now],
            )
            .map_err(map_rusqlite)?;
            tx.commit().map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(result_id)
    }

    async fn update_rule_status(
        &self,
        rule_id: &RuleId,
        kind: RuleKind,
        status: RuleStatus,
    ) -> Result<(), StorageError> {
        let (rules, _) = tables(kind);
        let id = rule_id.as_str().to_owned();
        let now = ts_to_db(Timestamp::now());
        self.backend.with_conn(|conn| {
            let sql = format!("UPDATE {rules} SET status = ?2, updated_at = ?3 WHERE id = ?1");
            conn.execute(&sql, params![id, status.as_str(), now])
                .map_err(map_rusqlite)?;
            Ok(())
        })
    }
}
