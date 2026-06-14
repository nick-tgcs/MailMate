//! SQLite-backed [`AuditRepository`] — the append-only cross-cutting provenance timeline.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Row};

use mailmate_common::actor::Actor;
use mailmate_common::audit::{AuditEntry, AuditQuery};
use mailmate_common::error::StorageError;
use mailmate_common::ids::{AuditId, MessageId, ProposalId, RuleId, RuleVersionId, ThreadId};
use mailmate_common::rules::rule::RuleKind;
use mailmate_ports::storage::audit::AuditRepository;

use crate::backend::{map_rusqlite, SqliteBackend};
use crate::repositories::convert::{json_from_db, json_to_db, ts_from_db, ts_to_db};

const AUDIT_COLUMNS: &str = "id, event_type, message_id, thread_id, rule_kind, rule_id, \
     rule_version_id, proposal_id, actor, payload_json, created_at";

/// SQLite implementation of [`AuditRepository`].
pub struct SqliteAuditRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteAuditRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

fn row_to_entry(row: &Row<'_>) -> Result<AuditEntry, StorageError> {
    let actor_raw: String = row.get(8).map_err(map_rusqlite)?;
    let actor = Actor::from_db_str(&actor_raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown actor {actor_raw:?}")))?;
    let rule_kind_raw: Option<String> = row.get(4).map_err(map_rusqlite)?;
    let rule_kind = match rule_kind_raw {
        Some(s) => Some(
            RuleKind::from_db_str(&s)
                .ok_or_else(|| StorageError::Serialization(format!("unknown rule kind {s:?}")))?,
        ),
        None => None,
    };
    let payload_raw: String = row.get(9).map_err(map_rusqlite)?;
    let created_at: String = row.get(10).map_err(map_rusqlite)?;
    let message_id: Option<String> = row.get(2).map_err(map_rusqlite)?;
    let thread_id: Option<String> = row.get(3).map_err(map_rusqlite)?;
    let rule_id: Option<String> = row.get(5).map_err(map_rusqlite)?;
    let rule_version_id: Option<String> = row.get(6).map_err(map_rusqlite)?;
    let proposal_id: Option<String> = row.get(7).map_err(map_rusqlite)?;

    Ok(AuditEntry {
        id: AuditId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        event_type: row.get(1).map_err(map_rusqlite)?,
        message_id: message_id.map(MessageId::from),
        thread_id: thread_id.map(ThreadId::from),
        rule_kind,
        rule_id: rule_id.map(RuleId::from),
        rule_version_id: rule_version_id.map(RuleVersionId::from),
        proposal_id: proposal_id.map(ProposalId::from),
        actor,
        payload: json_from_db(&payload_raw)?,
        created_at: ts_from_db(&created_at)?,
    })
}

#[async_trait]
impl AuditRepository for SqliteAuditRepository {
    async fn append(&self, entry: AuditEntry) -> Result<AuditId, StorageError> {
        let payload_json = json_to_db(&entry.payload)?;
        let id = entry.id.clone();
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO audit_log (id, event_type, message_id, thread_id, rule_kind, \
                 rule_id, rule_version_id, proposal_id, actor, payload_json, created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    entry.id.as_str(),
                    entry.event_type,
                    entry.message_id.as_ref().map(MessageId::as_str),
                    entry.thread_id.as_ref().map(ThreadId::as_str),
                    entry.rule_kind.map(RuleKind::as_str),
                    entry.rule_id.as_ref().map(RuleId::as_str),
                    entry.rule_version_id.as_ref().map(RuleVersionId::as_str),
                    entry.proposal_id.as_ref().map(ProposalId::as_str),
                    entry.actor.as_str(),
                    payload_json,
                    ts_to_db(entry.created_at),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn query(&self, query: AuditQuery) -> Result<Vec<AuditEntry>, StorageError> {
        self.backend.with_conn(|conn| {
            // Dynamic predicate list, all bound parameters (no string interpolation of values).
            let mut clauses: Vec<&str> = Vec::new();
            let mut binds: Vec<String> = Vec::new();
            if let Some(event_type) = &query.event_type {
                binds.push(event_type.clone());
                clauses.push("event_type");
            }
            if let Some(message_id) = &query.message_id {
                binds.push(message_id.as_str().to_owned());
                clauses.push("message_id");
            }
            if let Some(rule_id) = &query.rule_id {
                binds.push(rule_id.as_str().to_owned());
                clauses.push("rule_id");
            }
            let where_sql = if clauses.is_empty() {
                String::new()
            } else {
                let predicates: Vec<String> = clauses
                    .iter()
                    .enumerate()
                    .map(|(i, col)| format!("{col} = ?{}", i + 1))
                    .collect();
                format!("WHERE {}", predicates.join(" AND "))
            };
            let limit_sql = match query.limit {
                Some(n) => format!("LIMIT {n}"),
                None => String::new(),
            };
            let sql = format!(
                "SELECT {AUDIT_COLUMNS} FROM audit_log {where_sql} ORDER BY created_at DESC, id DESC {limit_sql}"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let bind_refs: Vec<&dyn rusqlite::ToSql> =
                binds.iter().map(|b| b as &dyn rusqlite::ToSql).collect();
            let mut rows = stmt.query(bind_refs.as_slice()).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_entry(row)?);
            }
            Ok(out)
        })
    }
}
