//! SQLite-backed [`FeedbackRepository`] — one struct, one `impl` per
//! [`TaskFeedbackKind`](mailmate_common::feedback::TaskFeedbackKind). The classification
//! and filing tables are the two live in Phase 7; a later kind (`followup_feedback`) adds
//! one more `impl` block over this same struct.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Row};

use mailmate_common::error::StorageError;
use mailmate_common::features::FeatureVector;
use mailmate_common::feedback::{
    ClassificationFeedback, ClassificationFeedbackQuery, ClassificationFeedbackRow,
    FeedbackPolarity, FilingFeedback, FilingFeedbackQuery, FilingFeedbackRow, FollowUpFeedback,
    FollowUpFeedbackQuery, FollowUpFeedbackRow, FollowUpOutcome, PinnedVersions, ProposalOutcome,
    RuleProposalFeedback, RuleProposalFeedbackQuery, RuleProposalFeedbackRow,
};
use mailmate_common::ids::{
    DraftId, FeedbackId, FolderId, MessageId, PipelineItemId, ProposalId, RuleId,
    WorkflowInstanceId,
};
use mailmate_ports::storage::feedback::FeedbackRepository;

use crate::backend::{map_rusqlite, SqliteBackend};
use crate::repositories::convert::{json_from_db, json_to_db, ts_from_db, ts_to_db};

/// SQLite implementation of the per-task feedback repositories.
pub struct SqliteFeedbackRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteFeedbackRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

fn polarity_from_db(raw: &str) -> Result<FeedbackPolarity, StorageError> {
    FeedbackPolarity::from_db_str(raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown polarity {raw:?}")))
}

fn proposal_outcome_from_db(raw: &str) -> Result<ProposalOutcome, StorageError> {
    ProposalOutcome::from_db_str(raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown proposal outcome {raw:?}")))
}

fn followup_outcome_from_db(raw: &str) -> Result<FollowUpOutcome, StorageError> {
    FollowUpOutcome::from_db_str(raw)
        .ok_or_else(|| StorageError::Serialization(format!("unknown followup outcome {raw:?}")))
}

const CLASSIFICATION_COLUMNS: &str = "id, message_id, pinned_versions_json, ai_label, ai_score, \
     ai_rationale, human_label, human_reason_code, human_reason_text, salient_features_json, \
     polarity, created_at";

fn row_to_classification(row: &Row<'_>) -> Result<ClassificationFeedbackRow, StorageError> {
    let pinned_raw: String = row.get(2).map_err(map_rusqlite)?;
    let features_raw: String = row.get(9).map_err(map_rusqlite)?;
    let polarity_raw: String = row.get(10).map_err(map_rusqlite)?;
    let created_at: String = row.get(11).map_err(map_rusqlite)?;
    Ok(ClassificationFeedbackRow {
        id: FeedbackId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        message_id: MessageId::from(row.get::<_, String>(1).map_err(map_rusqlite)?),
        pinned_versions: json_from_db::<PinnedVersions>(&pinned_raw)?,
        ai_label: row.get(3).map_err(map_rusqlite)?,
        ai_score: row.get(4).map_err(map_rusqlite)?,
        ai_rationale: row.get(5).map_err(map_rusqlite)?,
        human_label: row.get(6).map_err(map_rusqlite)?,
        human_reason_code: row.get(7).map_err(map_rusqlite)?,
        human_reason_text: row.get(8).map_err(map_rusqlite)?,
        salient_features: json_from_db::<FeatureVector>(&features_raw)?,
        polarity: polarity_from_db(&polarity_raw)?,
        created_at: ts_from_db(&created_at)?,
    })
}

const RULE_PROPOSAL_COLUMNS: &str = "id, proposal_id, pinned_versions_json, outcome, \
     human_reason_code, human_reason_text, polarity, created_at";

fn row_to_rule_proposal(row: &Row<'_>) -> Result<RuleProposalFeedbackRow, StorageError> {
    let pinned_raw: String = row.get(2).map_err(map_rusqlite)?;
    let outcome_raw: String = row.get(3).map_err(map_rusqlite)?;
    let polarity_raw: String = row.get(6).map_err(map_rusqlite)?;
    let created_at: String = row.get(7).map_err(map_rusqlite)?;
    Ok(RuleProposalFeedbackRow {
        id: FeedbackId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        proposal_id: ProposalId::from(row.get::<_, String>(1).map_err(map_rusqlite)?),
        pinned_versions: json_from_db::<PinnedVersions>(&pinned_raw)?,
        outcome: proposal_outcome_from_db(&outcome_raw)?,
        human_reason_code: row.get(4).map_err(map_rusqlite)?,
        human_reason_text: row.get(5).map_err(map_rusqlite)?,
        polarity: polarity_from_db(&polarity_raw)?,
        created_at: ts_from_db(&created_at)?,
    })
}

const FILING_COLUMNS: &str = "id, message_id, pinned_versions_json, sender_domain, \
     ai_suggested_folder, human_chosen_folder, basis, matched_rule_id, polarity, created_at";

fn row_to_filing(row: &Row<'_>) -> Result<FilingFeedbackRow, StorageError> {
    let pinned_raw: String = row.get(2).map_err(map_rusqlite)?;
    let ai_folder: Option<String> = row.get(4).map_err(map_rusqlite)?;
    let matched_rule: Option<String> = row.get(7).map_err(map_rusqlite)?;
    let polarity_raw: String = row.get(8).map_err(map_rusqlite)?;
    let created_at: String = row.get(9).map_err(map_rusqlite)?;
    Ok(FilingFeedbackRow {
        id: FeedbackId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        message_id: MessageId::from(row.get::<_, String>(1).map_err(map_rusqlite)?),
        pinned_versions: json_from_db::<PinnedVersions>(&pinned_raw)?,
        sender_domain: row.get(3).map_err(map_rusqlite)?,
        ai_suggested_folder: ai_folder.map(FolderId::from),
        human_chosen_folder: FolderId::from(row.get::<_, String>(5).map_err(map_rusqlite)?),
        basis: row.get(6).map_err(map_rusqlite)?,
        matched_rule_id: matched_rule.map(RuleId::from),
        polarity: polarity_from_db(&polarity_raw)?,
        created_at: ts_from_db(&created_at)?,
    })
}

const FOLLOWUP_COLUMNS: &str = "id, workflow_instance_id, pipeline_item_id, step_index, draft_id, \
     pinned_versions_json, ai_scheduled_offset_days, actual_offset_days, \
     reply_received_before_step, reply_latency_days, outcome, coalesced_from_json, \
     human_reason_code, human_reason_text, polarity, created_at";

fn row_to_followup(row: &Row<'_>) -> Result<FollowUpFeedbackRow, StorageError> {
    let draft_raw: Option<String> = row.get(4).map_err(map_rusqlite)?;
    let pinned_raw: String = row.get(5).map_err(map_rusqlite)?;
    let reply_before: i64 = row.get(8).map_err(map_rusqlite)?;
    let outcome_raw: String = row.get(10).map_err(map_rusqlite)?;
    let coalesced_raw: Option<String> = row.get(11).map_err(map_rusqlite)?;
    let polarity_raw: String = row.get(14).map_err(map_rusqlite)?;
    let created_at: String = row.get(15).map_err(map_rusqlite)?;
    let coalesced_from = match coalesced_raw {
        Some(s) => json_from_db::<Vec<i64>>(&s)?,
        None => Vec::new(),
    };
    Ok(FollowUpFeedbackRow {
        id: FeedbackId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        workflow_instance_id: WorkflowInstanceId::from(
            row.get::<_, String>(1).map_err(map_rusqlite)?,
        ),
        pipeline_item_id: PipelineItemId::from(row.get::<_, String>(2).map_err(map_rusqlite)?),
        step_index: row.get(3).map_err(map_rusqlite)?,
        draft_id: draft_raw.map(DraftId::from),
        pinned_versions: json_from_db::<PinnedVersions>(&pinned_raw)?,
        ai_scheduled_offset_days: row.get(6).map_err(map_rusqlite)?,
        actual_offset_days: row.get(7).map_err(map_rusqlite)?,
        reply_received_before_step: reply_before != 0,
        reply_latency_days: row.get(9).map_err(map_rusqlite)?,
        outcome: followup_outcome_from_db(&outcome_raw)?,
        coalesced_from,
        human_reason_code: row.get(12).map_err(map_rusqlite)?,
        human_reason_text: row.get(13).map_err(map_rusqlite)?,
        polarity: polarity_from_db(&polarity_raw)?,
        created_at: ts_from_db(&created_at)?,
    })
}

fn limit_clause(limit: Option<usize>) -> String {
    match limit {
        Some(n) => format!("LIMIT {n}"),
        None => String::new(),
    }
}

#[async_trait]
impl FeedbackRepository<ClassificationFeedback> for SqliteFeedbackRepository {
    async fn append(&self, row: ClassificationFeedbackRow) -> Result<FeedbackId, StorageError> {
        let pinned_json = json_to_db(&row.pinned_versions)?;
        let features_json = json_to_db(&row.salient_features)?;
        let id = row.id.clone();
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO classification_feedback (id, message_id, pinned_versions_json, \
                 ai_label, ai_score, ai_rationale, human_label, human_reason_code, \
                 human_reason_text, salient_features_json, polarity, created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    row.id.as_str(),
                    row.message_id.as_str(),
                    pinned_json,
                    row.ai_label,
                    row.ai_score,
                    row.ai_rationale,
                    row.human_label,
                    row.human_reason_code,
                    row.human_reason_text,
                    features_json,
                    row.polarity.as_str(),
                    ts_to_db(row.created_at),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn query(
        &self,
        query: ClassificationFeedbackQuery,
    ) -> Result<Vec<ClassificationFeedbackRow>, StorageError> {
        self.backend.with_conn(|conn| {
            let mut binds: Vec<String> = Vec::new();
            let mut predicates: Vec<String> = Vec::new();
            if let Some(message_id) = &query.message_id {
                binds.push(message_id.as_str().to_owned());
                predicates.push(format!("message_id = ?{}", binds.len()));
            }
            if let Some(label) = &query.human_label {
                binds.push(label.clone());
                predicates.push(format!("human_label = ?{}", binds.len()));
            }
            let where_sql = if predicates.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", predicates.join(" AND "))
            };
            let sql = format!(
                "SELECT {CLASSIFICATION_COLUMNS} FROM classification_feedback {where_sql} \
                 ORDER BY created_at DESC, id DESC {}",
                limit_clause(query.limit)
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let refs: Vec<&dyn rusqlite::ToSql> =
                binds.iter().map(|b| b as &dyn rusqlite::ToSql).collect();
            let mut rows = stmt.query(refs.as_slice()).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_classification(row)?);
            }
            Ok(out)
        })
    }
}

#[async_trait]
impl FeedbackRepository<FilingFeedback> for SqliteFeedbackRepository {
    async fn append(&self, row: FilingFeedbackRow) -> Result<FeedbackId, StorageError> {
        let pinned_json = json_to_db(&row.pinned_versions)?;
        let id = row.id.clone();
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO filing_feedback (id, message_id, pinned_versions_json, sender_domain, \
                 ai_suggested_folder, human_chosen_folder, basis, matched_rule_id, polarity, \
                 created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    row.id.as_str(),
                    row.message_id.as_str(),
                    pinned_json,
                    row.sender_domain,
                    row.ai_suggested_folder.as_ref().map(FolderId::as_str),
                    row.human_chosen_folder.as_str(),
                    row.basis,
                    row.matched_rule_id.as_ref().map(RuleId::as_str),
                    row.polarity.as_str(),
                    ts_to_db(row.created_at),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn query(
        &self,
        query: FilingFeedbackQuery,
    ) -> Result<Vec<FilingFeedbackRow>, StorageError> {
        self.backend.with_conn(|conn| {
            let mut binds: Vec<String> = Vec::new();
            let mut predicates: Vec<String> = Vec::new();
            if let Some(message_id) = &query.message_id {
                binds.push(message_id.as_str().to_owned());
                predicates.push(format!("message_id = ?{}", binds.len()));
            }
            if let Some(folder) = &query.human_chosen_folder {
                binds.push(folder.as_str().to_owned());
                predicates.push(format!("human_chosen_folder = ?{}", binds.len()));
            }
            let where_sql = if predicates.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", predicates.join(" AND "))
            };
            let sql = format!(
                "SELECT {FILING_COLUMNS} FROM filing_feedback {where_sql} \
                 ORDER BY created_at DESC, id DESC {}",
                limit_clause(query.limit)
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let refs: Vec<&dyn rusqlite::ToSql> =
                binds.iter().map(|b| b as &dyn rusqlite::ToSql).collect();
            let mut rows = stmt.query(refs.as_slice()).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_filing(row)?);
            }
            Ok(out)
        })
    }
}

#[async_trait]
impl FeedbackRepository<RuleProposalFeedback> for SqliteFeedbackRepository {
    async fn append(&self, row: RuleProposalFeedbackRow) -> Result<FeedbackId, StorageError> {
        let pinned_json = json_to_db(&row.pinned_versions)?;
        let id = row.id.clone();
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO rule_proposal_feedback (id, proposal_id, pinned_versions_json, \
                 outcome, human_reason_code, human_reason_text, polarity, created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    row.id.as_str(),
                    row.proposal_id.as_str(),
                    pinned_json,
                    row.outcome.as_str(),
                    row.human_reason_code,
                    row.human_reason_text,
                    row.polarity.as_str(),
                    ts_to_db(row.created_at),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn query(
        &self,
        query: RuleProposalFeedbackQuery,
    ) -> Result<Vec<RuleProposalFeedbackRow>, StorageError> {
        self.backend.with_conn(|conn| {
            let mut binds: Vec<String> = Vec::new();
            let mut predicates: Vec<String> = Vec::new();
            if let Some(proposal_id) = &query.proposal_id {
                binds.push(proposal_id.as_str().to_owned());
                predicates.push(format!("proposal_id = ?{}", binds.len()));
            }
            let where_sql = if predicates.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", predicates.join(" AND "))
            };
            let sql = format!(
                "SELECT {RULE_PROPOSAL_COLUMNS} FROM rule_proposal_feedback {where_sql} \
                 ORDER BY created_at DESC, id DESC {}",
                limit_clause(query.limit)
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let refs: Vec<&dyn rusqlite::ToSql> =
                binds.iter().map(|b| b as &dyn rusqlite::ToSql).collect();
            let mut rows = stmt.query(refs.as_slice()).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_rule_proposal(row)?);
            }
            Ok(out)
        })
    }
}

#[async_trait]
impl FeedbackRepository<FollowUpFeedback> for SqliteFeedbackRepository {
    async fn append(&self, row: FollowUpFeedbackRow) -> Result<FeedbackId, StorageError> {
        let pinned_json = json_to_db(&row.pinned_versions)?;
        let coalesced_json = if row.coalesced_from.is_empty() {
            None
        } else {
            Some(json_to_db(&row.coalesced_from)?)
        };
        let id = row.id.clone();
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO followup_feedback (id, workflow_instance_id, pipeline_item_id, \
                 step_index, draft_id, pinned_versions_json, ai_scheduled_offset_days, \
                 actual_offset_days, reply_received_before_step, reply_latency_days, outcome, \
                 coalesced_from_json, human_reason_code, human_reason_text, polarity, created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                params![
                    row.id.as_str(),
                    row.workflow_instance_id.as_str(),
                    row.pipeline_item_id.as_str(),
                    row.step_index,
                    row.draft_id.as_ref().map(DraftId::as_str),
                    pinned_json,
                    row.ai_scheduled_offset_days,
                    row.actual_offset_days,
                    i64::from(row.reply_received_before_step),
                    row.reply_latency_days,
                    row.outcome.as_str(),
                    coalesced_json,
                    row.human_reason_code,
                    row.human_reason_text,
                    row.polarity.as_str(),
                    ts_to_db(row.created_at),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn query(
        &self,
        query: FollowUpFeedbackQuery,
    ) -> Result<Vec<FollowUpFeedbackRow>, StorageError> {
        self.backend.with_conn(|conn| {
            let mut binds: Vec<String> = Vec::new();
            let mut predicates: Vec<String> = Vec::new();
            if let Some(instance_id) = &query.workflow_instance_id {
                binds.push(instance_id.as_str().to_owned());
                predicates.push(format!("workflow_instance_id = ?{}", binds.len()));
            }
            if let Some(item_id) = &query.pipeline_item_id {
                binds.push(item_id.as_str().to_owned());
                predicates.push(format!("pipeline_item_id = ?{}", binds.len()));
            }
            let where_sql = if predicates.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", predicates.join(" AND "))
            };
            let sql = format!(
                "SELECT {FOLLOWUP_COLUMNS} FROM followup_feedback {where_sql} \
                 ORDER BY created_at DESC, id DESC {}",
                limit_clause(query.limit)
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let refs: Vec<&dyn rusqlite::ToSql> =
                binds.iter().map(|b| b as &dyn rusqlite::ToSql).collect();
            let mut rows = stmt.query(refs.as_slice()).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_followup(row)?);
            }
            Ok(out)
        })
    }
}
