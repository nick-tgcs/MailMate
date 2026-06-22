//! SQLite-backed workflow repositories: the versioned-cadence definition store (mirroring
//! [`SqliteRuleRepository`](crate::repositories::rules::SqliteRuleRepository)'s
//! insert-rule + insert-version + repoint transaction), the mutable-state instance store,
//! and the containment-conflict and shadow-outcome append stores.
//!
//! `rusqlite` never escapes these methods — every one returns domain types and
//! [`StorageError`].

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Row};

use mailmate_common::actor::Actor;
use mailmate_common::error::StorageError;
use mailmate_common::ids::{
    PipelineItemId, ThreadId, WorkflowConflictId, WorkflowDefId, WorkflowDefVersionId,
    WorkflowInstanceId, WorkflowShadowOutcomeId,
};
use mailmate_common::pipeline::ItemType;
use mailmate_common::rules::condition::Condition;
use mailmate_common::rules::rule::{RiskLevel, RuleScope, RuleStatus};
use mailmate_common::time::Timestamp;
use mailmate_common::workflow::{
    ExitCondition, FollowUpStep, NewWorkflowDefVersion, NewWorkflowDefinition, NewWorkflowInstance,
    Staleness, WorkflowAnchor, WorkflowConflict, WorkflowConflictKind, WorkflowConflictStatus,
    WorkflowDefinition, WorkflowDefinitionVersion, WorkflowInstance, WorkflowInstanceStatus,
    WorkflowShadowOutcome, WorkflowVersionContent,
};
use mailmate_ports::storage::workflows::{
    WorkflowConflictRepository, WorkflowInstanceRepository, WorkflowRepository,
    WorkflowShadowOutcomeRepository,
};

use crate::backend::{map_rusqlite, SqliteBackend};
use crate::repositories::convert::{json_from_db, json_to_db, ts_from_db, ts_to_db};

// --- shared parse helpers -------------------------------------------------------------

fn scope_from_db(raw: &str) -> Result<RuleScope, StorageError> {
    RuleScope::from_db_str(raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown rule scope {raw:?}")))
}

fn status_from_db(raw: &str) -> Result<RuleStatus, StorageError> {
    RuleStatus::from_db_str(raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown rule status {raw:?}")))
}

fn item_type_from_db(raw: &str) -> Result<ItemType, StorageError> {
    ItemType::from_db_str(raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown item type {raw:?}")))
}

fn anchor_from_db(raw: &str) -> Result<WorkflowAnchor, StorageError> {
    WorkflowAnchor::from_db_str(raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown workflow anchor {raw:?}")))
}

fn risk_from_db(raw: &str) -> Result<RiskLevel, StorageError> {
    RiskLevel::from_db_str(raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown risk level {raw:?}")))
}

fn actor_from_db(raw: &str) -> Result<Actor, StorageError> {
    Actor::from_db_str(raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown actor {raw:?}")))
}

fn instance_status_from_db(raw: &str) -> Result<WorkflowInstanceStatus, StorageError> {
    WorkflowInstanceStatus::from_db_str(raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown instance status {raw:?}")))
}

// --- definitions / versions -----------------------------------------------------------

/// SQLite implementation of [`WorkflowRepository`].
pub struct SqliteWorkflowRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteWorkflowRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

const DEFINITION_COLUMNS: &str = "id, stable_name, scope, applies_to_item_type, status, \
     current_version_id, created_by, created_at, updated_at";

fn row_to_definition(row: &Row<'_>) -> Result<WorkflowDefinition, StorageError> {
    let scope_raw: String = row.get(2).map_err(map_rusqlite)?;
    let item_type_raw: String = row.get(3).map_err(map_rusqlite)?;
    let status_raw: String = row.get(4).map_err(map_rusqlite)?;
    let current_version: Option<String> = row.get(5).map_err(map_rusqlite)?;
    let created_by_raw: String = row.get(6).map_err(map_rusqlite)?;
    let created_at: String = row.get(7).map_err(map_rusqlite)?;
    let updated_at: String = row.get(8).map_err(map_rusqlite)?;
    let current_version_id = current_version.ok_or_else(|| {
        StorageError::Serialization("workflow definition has no current version".to_owned())
    })?;
    Ok(WorkflowDefinition {
        id: WorkflowDefId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        stable_name: row.get(1).map_err(map_rusqlite)?,
        scope: scope_from_db(&scope_raw)?,
        applies_to_item_type: item_type_from_db(&item_type_raw)?,
        status: status_from_db(&status_raw)?,
        current_version_id: WorkflowDefVersionId::from(current_version_id),
        created_by: actor_from_db(&created_by_raw)?,
        created_at: ts_from_db(&created_at)?,
        updated_at: ts_from_db(&updated_at)?,
    })
}

const VERSION_COLUMNS: &str = "id, workflow_id, version_number, title, description, anchor, \
     enrollment_condition_json, steps_json, exit_conditions_json, staleness_json, risk_level, \
     created_by, change_reason, created_at";

fn row_to_version(row: &Row<'_>) -> Result<WorkflowDefinitionVersion, StorageError> {
    let anchor_raw: String = row.get(5).map_err(map_rusqlite)?;
    let enrollment_raw: Option<String> = row.get(6).map_err(map_rusqlite)?;
    let steps_raw: String = row.get(7).map_err(map_rusqlite)?;
    let exits_raw: String = row.get(8).map_err(map_rusqlite)?;
    let staleness_raw: String = row.get(9).map_err(map_rusqlite)?;
    let risk_raw: String = row.get(10).map_err(map_rusqlite)?;
    let created_by_raw: String = row.get(11).map_err(map_rusqlite)?;
    let created_at: String = row.get(13).map_err(map_rusqlite)?;
    let enrollment_condition = match enrollment_raw {
        Some(s) => Some(json_from_db::<Condition>(&s)?),
        None => None,
    };
    Ok(WorkflowDefinitionVersion {
        id: WorkflowDefVersionId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        workflow_id: WorkflowDefId::from(row.get::<_, String>(1).map_err(map_rusqlite)?),
        version_number: row.get(2).map_err(map_rusqlite)?,
        content: WorkflowVersionContent {
            title: row.get(3).map_err(map_rusqlite)?,
            description: row.get(4).map_err(map_rusqlite)?,
            anchor: anchor_from_db(&anchor_raw)?,
            enrollment_condition,
            steps: json_from_db::<Vec<FollowUpStep>>(&steps_raw)?,
            exit_conditions: json_from_db::<Vec<ExitCondition>>(&exits_raw)?,
            staleness: json_from_db::<Staleness>(&staleness_raw)?,
            risk_level: risk_from_db(&risk_raw)?,
            change_reason: row.get(12).map_err(map_rusqlite)?,
            created_by: actor_from_db(&created_by_raw)?,
        },
        created_at: ts_from_db(&created_at)?,
    })
}

/// Insert one version row inside an open transaction; the caller supplies the id + number.
fn insert_version(
    conn: &rusqlite::Connection,
    version_id: &WorkflowDefVersionId,
    workflow_id: &WorkflowDefId,
    version_number: i64,
    content: &WorkflowVersionContent,
) -> Result<(), StorageError> {
    let enrollment_json = match &content.enrollment_condition {
        Some(c) => Some(json_to_db(c)?),
        None => None,
    };
    let steps_json = json_to_db(&content.steps)?;
    let exits_json = json_to_db(&content.exit_conditions)?;
    let staleness_json = json_to_db(&content.staleness)?;
    conn.execute(
        "INSERT INTO workflow_definition_versions (id, workflow_id, version_number, title, \
         description, anchor, enrollment_condition_json, steps_json, exit_conditions_json, \
         staleness_json, risk_level, created_by, change_reason, created_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![
            version_id.as_str(),
            workflow_id.as_str(),
            version_number,
            content.title,
            content.description,
            content.anchor.as_str(),
            enrollment_json,
            steps_json,
            exits_json,
            staleness_json,
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
impl WorkflowRepository for SqliteWorkflowRepository {
    async fn save_definition_draft(
        &self,
        draft: NewWorkflowDefinition,
    ) -> Result<WorkflowDefId, StorageError> {
        let workflow_id = WorkflowDefId::fresh();
        let version_id = WorkflowDefVersionId::fresh();
        let now = ts_to_db(Timestamp::now());
        let result = workflow_id.clone();
        self.backend.with_conn_mut(|conn| {
            let tx = conn.transaction().map_err(map_rusqlite)?;
            tx.execute(
                "INSERT INTO workflow_definitions (id, stable_name, scope, applies_to_item_type, \
                 status, current_version_id, created_by, created_at, updated_at) \
                 VALUES (?1,?2,?3,?4,?5,NULL,?6,?7,?7)",
                params![
                    workflow_id.as_str(),
                    draft.stable_name,
                    draft.scope.as_str(),
                    draft.applies_to_item_type.as_str(),
                    RuleStatus::Draft.as_str(),
                    draft.created_by.as_str(),
                    now,
                ],
            )
            .map_err(map_rusqlite)?;
            insert_version(&tx, &version_id, &workflow_id, 1, &draft.initial_version)?;
            tx.execute(
                "UPDATE workflow_definitions SET current_version_id = ?2 WHERE id = ?1",
                params![workflow_id.as_str(), version_id.as_str()],
            )
            .map_err(map_rusqlite)?;
            tx.commit().map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(result)
    }

    async fn create_version(
        &self,
        version: NewWorkflowDefVersion,
    ) -> Result<WorkflowDefVersionId, StorageError> {
        let version_id = WorkflowDefVersionId::fresh();
        let now = ts_to_db(Timestamp::now());
        let result = version_id.clone();
        self.backend.with_conn_mut(|conn| {
            let tx = conn.transaction().map_err(map_rusqlite)?;
            let next: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(version_number), 0) FROM workflow_definition_versions \
                     WHERE workflow_id = ?1",
                    params![version.workflow_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(map_rusqlite)?;
            insert_version(
                &tx,
                &version_id,
                &version.workflow_id,
                next + 1,
                &version.content,
            )?;
            tx.execute(
                "UPDATE workflow_definitions SET current_version_id = ?2, updated_at = ?3 \
                 WHERE id = ?1",
                params![version.workflow_id.as_str(), version_id.as_str(), now],
            )
            .map_err(map_rusqlite)?;
            tx.commit().map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(result)
    }

    async fn update_status(
        &self,
        id: &WorkflowDefId,
        status: RuleStatus,
    ) -> Result<(), StorageError> {
        let now = ts_to_db(Timestamp::now());
        self.backend.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_definitions SET status = ?2, updated_at = ?3 WHERE id = ?1",
                params![id.as_str(), status.as_str(), now],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }

    async fn get_definition(
        &self,
        id: &WorkflowDefId,
    ) -> Result<Option<WorkflowDefinition>, StorageError> {
        self.backend.with_conn(|conn| {
            let sql =
                format!("SELECT {DEFINITION_COLUMNS} FROM workflow_definitions WHERE id = ?1");
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![id.as_str()]).map_err(map_rusqlite)?;
            match rows.next().map_err(map_rusqlite)? {
                Some(row) => Ok(Some(row_to_definition(row)?)),
                None => Ok(None),
            }
        })
    }

    async fn get_version(
        &self,
        id: &WorkflowDefVersionId,
    ) -> Result<Option<WorkflowDefinitionVersion>, StorageError> {
        self.backend.with_conn(|conn| {
            let sql =
                format!("SELECT {VERSION_COLUMNS} FROM workflow_definition_versions WHERE id = ?1");
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![id.as_str()]).map_err(map_rusqlite)?;
            match rows.next().map_err(map_rusqlite)? {
                Some(row) => Ok(Some(row_to_version(row)?)),
                None => Ok(None),
            }
        })
    }

    async fn list_by_status(
        &self,
        status: RuleStatus,
    ) -> Result<Vec<WorkflowDefinition>, StorageError> {
        self.backend.with_conn(|conn| {
            let sql = format!(
                "SELECT {DEFINITION_COLUMNS} FROM workflow_definitions WHERE status = ?1 \
                 ORDER BY created_at, id"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![status.as_str()]).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_definition(row)?);
            }
            Ok(out)
        })
    }
}

// --- instances ------------------------------------------------------------------------

/// SQLite implementation of [`WorkflowInstanceRepository`].
pub struct SqliteWorkflowInstanceRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteWorkflowInstanceRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

const INSTANCE_COLUMNS: &str = "id, pipeline_item_id, workflow_id, pinned_def_version_id, \
     thread_id, anchor_at, status, current_step_index, next_due_at, created_at, updated_at";

fn row_to_instance(row: &Row<'_>) -> Result<WorkflowInstance, StorageError> {
    let anchor_at: String = row.get(5).map_err(map_rusqlite)?;
    let status_raw: String = row.get(6).map_err(map_rusqlite)?;
    let next_due_raw: Option<String> = row.get(8).map_err(map_rusqlite)?;
    let created_at: String = row.get(9).map_err(map_rusqlite)?;
    let updated_at: String = row.get(10).map_err(map_rusqlite)?;
    let next_due_at = match next_due_raw {
        Some(s) => Some(ts_from_db(&s)?),
        None => None,
    };
    Ok(WorkflowInstance {
        id: WorkflowInstanceId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        pipeline_item_id: PipelineItemId::from(row.get::<_, String>(1).map_err(map_rusqlite)?),
        workflow_id: WorkflowDefId::from(row.get::<_, String>(2).map_err(map_rusqlite)?),
        pinned_def_version_id: WorkflowDefVersionId::from(
            row.get::<_, String>(3).map_err(map_rusqlite)?,
        ),
        thread_id: ThreadId::from(row.get::<_, String>(4).map_err(map_rusqlite)?),
        anchor_at: ts_from_db(&anchor_at)?,
        status: instance_status_from_db(&status_raw)?,
        current_step_index: row.get(7).map_err(map_rusqlite)?,
        next_due_at,
        created_at: ts_from_db(&created_at)?,
        updated_at: ts_from_db(&updated_at)?,
    })
}

#[async_trait]
impl WorkflowInstanceRepository for SqliteWorkflowInstanceRepository {
    async fn arm(&self, instance: NewWorkflowInstance) -> Result<WorkflowInstanceId, StorageError> {
        let id = WorkflowInstanceId::fresh();
        let now = ts_to_db(Timestamp::now());
        let result = id.clone();
        // Store the trigger at whole-second precision so the `next_due_at <= now` TEXT
        // comparison in `list_due` is lexicographically exact (RFC3339 renders sub-seconds
        // only when non-zero, which would otherwise mis-order same-second values).
        let next_due = instance.next_due_at.map(|t| ts_to_db(t.floor_to_seconds()));
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO workflow_instances (id, pipeline_item_id, workflow_id, \
                 pinned_def_version_id, thread_id, anchor_at, status, current_step_index, \
                 next_due_at, created_at, updated_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",
                params![
                    id.as_str(),
                    instance.pipeline_item_id.as_str(),
                    instance.workflow_id.as_str(),
                    instance.pinned_def_version_id.as_str(),
                    instance.thread_id.as_str(),
                    ts_to_db(instance.anchor_at),
                    instance.status.as_str(),
                    instance.current_step_index,
                    next_due,
                    now,
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(result)
    }

    async fn get(&self, id: &WorkflowInstanceId) -> Result<Option<WorkflowInstance>, StorageError> {
        self.backend.with_conn(|conn| {
            let sql = format!("SELECT {INSTANCE_COLUMNS} FROM workflow_instances WHERE id = ?1");
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![id.as_str()]).map_err(map_rusqlite)?;
            match rows.next().map_err(map_rusqlite)? {
                Some(row) => Ok(Some(row_to_instance(row)?)),
                None => Ok(None),
            }
        })
    }

    async fn list_due(
        &self,
        now: Timestamp,
        limit: usize,
    ) -> Result<Vec<WorkflowInstance>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        // Compare at whole-second precision (matching how `next_due_at` is stored), so the
        // fixed-width `YYYY-MM-DDTHH:MM:SSZ` strings sort lexicographically by instant.
        let now_db = ts_to_db(now.floor_to_seconds());
        self.backend.with_conn(|conn| {
            // The load-bearing predicate, served by `idx_workflow_instances_status_next_due`.
            // Both sides are whole-second RFC3339 UTC, so `<=` on TEXT is exact. `LIMIT` bounds
            // a long-offline catch-up so the soonest-due batch drains first, the rest next tick.
            let sql = format!(
                "SELECT {INSTANCE_COLUMNS} FROM workflow_instances \
                 WHERE status IN ('active','snoozed') AND next_due_at IS NOT NULL \
                 AND next_due_at <= ?1 ORDER BY next_due_at ASC, id ASC LIMIT ?2"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt
                .query(params![now_db, limit as i64])
                .map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_instance(row)?);
            }
            Ok(out)
        })
    }

    async fn list_active_by_thread(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Vec<WorkflowInstance>, StorageError> {
        self.backend.with_conn(|conn| {
            let sql = format!(
                "SELECT {INSTANCE_COLUMNS} FROM workflow_instances \
                 WHERE thread_id = ?1 AND status NOT IN ('completed','cancelled') \
                 ORDER BY created_at, id"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt
                .query(params![thread_id.as_str()])
                .map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_instance(row)?);
            }
            Ok(out)
        })
    }

    async fn list_by_pipeline_item(
        &self,
        pipeline_item_id: &PipelineItemId,
    ) -> Result<Vec<WorkflowInstance>, StorageError> {
        self.backend.with_conn(|conn| {
            let sql = format!(
                "SELECT {INSTANCE_COLUMNS} FROM workflow_instances WHERE pipeline_item_id = ?1 \
                 ORDER BY created_at, id"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt
                .query(params![pipeline_item_id.as_str()])
                .map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_instance(row)?);
            }
            Ok(out)
        })
    }

    async fn update_state(
        &self,
        id: &WorkflowInstanceId,
        status: WorkflowInstanceStatus,
        current_step_index: i64,
        next_due_at: Option<Timestamp>,
    ) -> Result<(), StorageError> {
        let now = ts_to_db(Timestamp::now());
        // Whole-second precision keeps `list_due`'s TEXT comparison exact (see `arm`).
        let next_due = next_due_at.map(|t| ts_to_db(t.floor_to_seconds()));
        self.backend.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_instances SET status = ?2, current_step_index = ?3, \
                 next_due_at = ?4, updated_at = ?5 WHERE id = ?1",
                params![
                    id.as_str(),
                    status.as_str(),
                    current_step_index,
                    next_due,
                    now
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }
}

// --- conflicts ------------------------------------------------------------------------

/// SQLite implementation of [`WorkflowConflictRepository`].
pub struct SqliteWorkflowConflictRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteWorkflowConflictRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

const CONFLICT_COLUMNS: &str =
    "id, pipeline_item_id, workflow_a_id, workflow_b_id, conflict_kind, \
     status, detected_at";

fn row_to_conflict(row: &Row<'_>) -> Result<WorkflowConflict, StorageError> {
    let kind_raw: String = row.get(4).map_err(map_rusqlite)?;
    let status_raw: String = row.get(5).map_err(map_rusqlite)?;
    let detected_at: String = row.get(6).map_err(map_rusqlite)?;
    Ok(WorkflowConflict {
        id: WorkflowConflictId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        pipeline_item_id: PipelineItemId::from(row.get::<_, String>(1).map_err(map_rusqlite)?),
        workflow_a_id: row.get(2).map_err(map_rusqlite)?,
        workflow_b_id: row.get(3).map_err(map_rusqlite)?,
        conflict_kind: WorkflowConflictKind::from_db_str(&kind_raw).ok_or_else(|| {
            StorageError::Serialization(format!("unknown conflict kind {kind_raw:?}"))
        })?,
        status: WorkflowConflictStatus::from_db_str(&status_raw).ok_or_else(|| {
            StorageError::Serialization(format!("unknown conflict status {status_raw:?}"))
        })?,
        detected_at: ts_from_db(&detected_at)?,
    })
}

#[async_trait]
impl WorkflowConflictRepository for SqliteWorkflowConflictRepository {
    async fn append(&self, conflict: WorkflowConflict) -> Result<WorkflowConflictId, StorageError> {
        let id = conflict.id.clone();
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO workflow_conflicts (id, pipeline_item_id, workflow_a_id, \
                 workflow_b_id, conflict_kind, status, detected_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    conflict.id.as_str(),
                    conflict.pipeline_item_id.as_str(),
                    conflict.workflow_a_id,
                    conflict.workflow_b_id,
                    conflict.conflict_kind.as_str(),
                    conflict.status.as_str(),
                    ts_to_db(conflict.detected_at),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn list_open(&self) -> Result<Vec<WorkflowConflict>, StorageError> {
        self.backend.with_conn(|conn| {
            let sql = format!(
                "SELECT {CONFLICT_COLUMNS} FROM workflow_conflicts WHERE status = 'open' \
                 ORDER BY detected_at DESC, id DESC"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query([]).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_conflict(row)?);
            }
            Ok(out)
        })
    }

    async fn resolve(&self, id: &WorkflowConflictId) -> Result<(), StorageError> {
        self.backend.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_conflicts SET status = ?2 WHERE id = ?1",
                params![id.as_str(), WorkflowConflictStatus::Resolved.as_str()],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }
}

// --- shadow outcomes ------------------------------------------------------------------

/// SQLite implementation of [`WorkflowShadowOutcomeRepository`].
pub struct SqliteWorkflowShadowOutcomeRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteWorkflowShadowOutcomeRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

const SHADOW_COLUMNS: &str = "id, workflow_id, workflow_version_id, pipeline_item_id, thread_id, \
     step_index, would_fire_at, reply_before_fire, matched_manual_followup_within_days, created_at";

fn row_to_shadow(row: &Row<'_>) -> Result<WorkflowShadowOutcome, StorageError> {
    let would_fire: String = row.get(6).map_err(map_rusqlite)?;
    let reply_before: i64 = row.get(7).map_err(map_rusqlite)?;
    let created_at: String = row.get(9).map_err(map_rusqlite)?;
    Ok(WorkflowShadowOutcome {
        id: WorkflowShadowOutcomeId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        workflow_id: WorkflowDefId::from(row.get::<_, String>(1).map_err(map_rusqlite)?),
        workflow_version_id: WorkflowDefVersionId::from(
            row.get::<_, String>(2).map_err(map_rusqlite)?,
        ),
        pipeline_item_id: PipelineItemId::from(row.get::<_, String>(3).map_err(map_rusqlite)?),
        thread_id: ThreadId::from(row.get::<_, String>(4).map_err(map_rusqlite)?),
        step_index: row.get(5).map_err(map_rusqlite)?,
        would_fire_at: ts_from_db(&would_fire)?,
        reply_before_fire: reply_before != 0,
        matched_manual_followup_within_days: row.get(8).map_err(map_rusqlite)?,
        created_at: ts_from_db(&created_at)?,
    })
}

#[async_trait]
impl WorkflowShadowOutcomeRepository for SqliteWorkflowShadowOutcomeRepository {
    async fn append(
        &self,
        row: WorkflowShadowOutcome,
    ) -> Result<WorkflowShadowOutcomeId, StorageError> {
        let id = row.id.clone();
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO workflow_shadow_outcomes (id, workflow_id, workflow_version_id, \
                 pipeline_item_id, thread_id, step_index, would_fire_at, reply_before_fire, \
                 matched_manual_followup_within_days, created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    row.id.as_str(),
                    row.workflow_id.as_str(),
                    row.workflow_version_id.as_str(),
                    row.pipeline_item_id.as_str(),
                    row.thread_id.as_str(),
                    row.step_index,
                    ts_to_db(row.would_fire_at),
                    i64::from(row.reply_before_fire),
                    row.matched_manual_followup_within_days,
                    ts_to_db(row.created_at),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn list_for_workflow(
        &self,
        workflow_id: &WorkflowDefId,
    ) -> Result<Vec<WorkflowShadowOutcome>, StorageError> {
        self.backend.with_conn(|conn| {
            let sql = format!(
                "SELECT {SHADOW_COLUMNS} FROM workflow_shadow_outcomes WHERE workflow_id = ?1 \
                 ORDER BY created_at, id"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt
                .query(params![workflow_id.as_str()])
                .map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_shadow(row)?);
            }
            Ok(out)
        })
    }
}
