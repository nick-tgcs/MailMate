//! SQLite-backed [`ProposalRepository`].
//!
//! A proposal is stored whole in `proposal_json`; the columns that are queried or mutated
//! (`status`, `reviewed_at`, the `proposal_type`/`risk_level`/`target_*` filters) are
//! mirrored out so reads can filter without parsing JSON. The mutable bits — `status` and
//! `reviewed_at` — are read back from their **columns**, which are authoritative over the
//! (now-stale) copy embedded in `proposal_json`. The proposal and its evidence links are
//! written in one transaction so the supporting set is never orphaned.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Row};

use mailmate_common::error::StorageError;
use mailmate_common::evidence::{EvidenceKind, EvidenceSourceKind, RuleEvidence};
use mailmate_common::ids::{EvidenceId, FeedbackId, MessageId, ProposalId, RuleId};
use mailmate_common::proposal::{AgentProposal, ProposalStatus};
use mailmate_common::rules::rule::RuleKind;
use mailmate_common::time::Timestamp;
use mailmate_ports::storage::proposals::ProposalRepository;

use crate::backend::{map_rusqlite, SqliteBackend};
use crate::repositories::convert::{json_from_db, json_to_db, ts_from_db, ts_to_db};

/// SQLite implementation of [`ProposalRepository`].
pub struct SqliteProposalRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteProposalRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

fn insert_evidence(
    conn: &rusqlite::Connection,
    proposal_id: &ProposalId,
    evidence: &RuleEvidence,
) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO rule_evidence (id, rule_kind, rule_id, proposal_id, source_kind, source_id, \
         message_id, evidence_kind, weight, summary, created_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            evidence.id.as_str(),
            evidence.rule_kind.map(RuleKind::as_str),
            evidence.rule_id.as_ref().map(RuleId::as_str),
            // The proposal link is authoritative here (the in-memory item may not carry it).
            proposal_id.as_str(),
            evidence.source_kind.as_str(),
            evidence.source_id.as_str(),
            evidence.message_id.as_ref().map(MessageId::as_str),
            evidence.evidence_kind.as_str(),
            evidence.weight,
            evidence.summary,
            ts_to_db(evidence.created_at),
        ],
    )
    .map_err(map_rusqlite)?;
    Ok(())
}

fn row_to_evidence(row: &Row<'_>) -> Result<RuleEvidence, StorageError> {
    let rule_kind_raw: Option<String> = row.get(1).map_err(map_rusqlite)?;
    let rule_kind = match rule_kind_raw {
        Some(s) => Some(
            RuleKind::from_db_str(&s)
                .ok_or_else(|| StorageError::Serialization(format!("unknown rule kind {s:?}")))?,
        ),
        None => None,
    };
    let rule_id: Option<String> = row.get(2).map_err(map_rusqlite)?;
    let proposal_id: Option<String> = row.get(3).map_err(map_rusqlite)?;
    let source_kind_raw: String = row.get(4).map_err(map_rusqlite)?;
    let message_id: Option<String> = row.get(6).map_err(map_rusqlite)?;
    let evidence_kind_raw: String = row.get(7).map_err(map_rusqlite)?;
    let created_at: String = row.get(10).map_err(map_rusqlite)?;
    Ok(RuleEvidence {
        id: EvidenceId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        rule_kind,
        rule_id: rule_id.map(RuleId::from),
        proposal_id: proposal_id.map(ProposalId::from),
        source_kind: EvidenceSourceKind::from_db_str(&source_kind_raw).ok_or_else(|| {
            StorageError::Serialization(format!("unknown source kind {source_kind_raw:?}"))
        })?,
        source_id: FeedbackId::from(row.get::<_, String>(5).map_err(map_rusqlite)?),
        message_id: message_id.map(MessageId::from),
        evidence_kind: EvidenceKind::from_db_str(&evidence_kind_raw).ok_or_else(|| {
            StorageError::Serialization(format!("unknown evidence kind {evidence_kind_raw:?}"))
        })?,
        weight: row.get(8).map_err(map_rusqlite)?,
        summary: row.get(9).map_err(map_rusqlite)?,
        created_at: ts_from_db(&created_at)?,
    })
}

/// Rebuild a proposal from its JSON, then override the mutable fields from their columns.
fn row_to_proposal(row: &Row<'_>) -> Result<AgentProposal, StorageError> {
    let proposal_json: String = row.get(0).map_err(map_rusqlite)?;
    let status_raw: String = row.get(1).map_err(map_rusqlite)?;
    let reviewed_at: Option<String> = row.get(2).map_err(map_rusqlite)?;
    let mut proposal: AgentProposal = json_from_db(&proposal_json)?;
    proposal.status = ProposalStatus::from_db_str(&status_raw).ok_or_else(|| {
        StorageError::Serialization(format!("unknown proposal status {status_raw:?}"))
    })?;
    proposal.reviewed_at = match reviewed_at {
        Some(s) => Some(ts_from_db(&s)?),
        None => None,
    };
    Ok(proposal)
}

#[async_trait]
impl ProposalRepository for SqliteProposalRepository {
    async fn save(
        &self,
        proposal: AgentProposal,
        evidence: Vec<RuleEvidence>,
    ) -> Result<ProposalId, StorageError> {
        let proposal_json = json_to_db(&proposal)?;
        let id = proposal.id.clone();
        self.backend.with_conn_mut(|conn| {
            let tx = conn.transaction().map_err(map_rusqlite)?;
            tx.execute(
                "INSERT INTO agent_proposals (id, proposal_type, status, title, rationale, \
                 risk_level, recommended_status, proposal_json, target_rule_kind, target_rule_id, \
                 source_provider, created_at, reviewed_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    proposal.id.as_str(),
                    proposal.proposal_type.as_str(),
                    proposal.status.as_str(),
                    proposal.title,
                    proposal.rationale,
                    proposal.risk_level.as_str(),
                    proposal.recommended_status.as_str(),
                    proposal_json,
                    proposal.target_rule_kind.map(RuleKind::as_str),
                    proposal.target_rule_id.as_ref().map(RuleId::as_str),
                    proposal.source_provider,
                    ts_to_db(proposal.created_at),
                    proposal.reviewed_at.map(ts_to_db),
                ],
            )
            .map_err(map_rusqlite)?;
            for item in &evidence {
                insert_evidence(&tx, &proposal.id, item)?;
            }
            tx.commit().map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn get(&self, id: &ProposalId) -> Result<Option<AgentProposal>, StorageError> {
        let id = id.as_str().to_owned();
        self.backend.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT proposal_json, status, reviewed_at FROM agent_proposals WHERE id = ?1",
                )
                .map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![id]).map_err(map_rusqlite)?;
            match rows.next().map_err(map_rusqlite)? {
                Some(row) => Ok(Some(row_to_proposal(row)?)),
                None => Ok(None),
            }
        })
    }

    async fn list_by_status(
        &self,
        status: ProposalStatus,
    ) -> Result<Vec<AgentProposal>, StorageError> {
        self.backend.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT proposal_json, status, reviewed_at FROM agent_proposals \
                     WHERE status = ?1 ORDER BY created_at DESC, id DESC",
                )
                .map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![status.as_str()]).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_proposal(row)?);
            }
            Ok(out)
        })
    }

    async fn set_status(
        &self,
        id: &ProposalId,
        status: ProposalStatus,
        reviewed_at: Option<Timestamp>,
    ) -> Result<(), StorageError> {
        let id = id.as_str().to_owned();
        let reviewed = reviewed_at.map(ts_to_db);
        self.backend.with_conn(|conn| {
            conn.execute(
                "UPDATE agent_proposals SET status = ?2, reviewed_at = ?3 WHERE id = ?1",
                params![id, status.as_str(), reviewed],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }

    async fn evidence_for(&self, id: &ProposalId) -> Result<Vec<RuleEvidence>, StorageError> {
        let id = id.as_str().to_owned();
        self.backend.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, rule_kind, rule_id, proposal_id, source_kind, source_id, \
                     message_id, evidence_kind, weight, summary, created_at \
                     FROM rule_evidence WHERE proposal_id = ?1 ORDER BY created_at, id",
                )
                .map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![id]).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_evidence(row)?);
            }
            Ok(out)
        })
    }
}
