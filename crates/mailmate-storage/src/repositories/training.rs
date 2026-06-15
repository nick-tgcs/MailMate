//! SQLite-backed training-layer repositories: datasets, adapters, and evaluation runs.
//!
//! These persist the three durable training tables (there is no `training_examples` table —
//! examples are derived on export). An adapter is inserted at its registered status and only
//! [`AdapterRepository::set_status`] transitions it, so promotion to `active` is always an
//! explicit, separate write the evaluation gate drives.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Row};

use mailmate_common::error::StorageError;
use mailmate_common::ids::{AdapterId, DatasetId, EvalRunId};
use mailmate_common::training::{
    AdapterFormat, AdapterStatus, AdapterType, DatasetType, EvalMetrics, ExportFormat,
    ExportPrivacyLevel, LoraAdapterRecord, LoraEvalRunRecord, TrainingDatasetRecord,
};
use mailmate_ports::storage::{AdapterRepository, DatasetRepository, EvalRunRepository};

use crate::backend::{map_rusqlite, SqliteBackend};
use crate::repositories::convert::{json_from_db, json_to_db, ts_from_db, ts_to_db};

const DATASET_COLUMNS: &str = "id, name, dataset_type, base_model_family, example_ids_hash, \
     positive_count, negative_count, validation_count, test_count, privacy_level, \
     export_format, artifact_path, created_at";

const ADAPTER_COLUMNS: &str = "id, name, adapter_type, format, base_model_family, \
     base_model_name, base_model_revision, tokenizer_hash, chat_template_hash, \
     training_dataset_id, artifact_path, status, created_at";

const EVAL_COLUMNS: &str = "id, adapter_id, dataset_id, base_provider_id, metrics_json, \
     safety_failures, quality_score, approved_for_use, created_at";

fn enum_err(field: &str, raw: &str) -> StorageError {
    StorageError::Serialization(format!("unknown {field} {raw:?}"))
}

#[allow(clippy::cast_sign_loss)]
fn count_from(row: &Row<'_>, idx: usize) -> Result<usize, StorageError> {
    let n: i64 = row.get(idx).map_err(map_rusqlite)?;
    Ok(n as usize)
}

// ---- datasets --------------------------------------------------------------

/// SQLite implementation of [`DatasetRepository`].
pub struct SqliteDatasetRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteDatasetRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

#[allow(clippy::cast_possible_wrap)]
fn row_to_dataset(row: &Row<'_>) -> Result<TrainingDatasetRecord, StorageError> {
    let dataset_type_raw: String = row.get(2).map_err(map_rusqlite)?;
    let privacy_raw: String = row.get(9).map_err(map_rusqlite)?;
    let format_raw: String = row.get(10).map_err(map_rusqlite)?;
    let created_at: String = row.get(12).map_err(map_rusqlite)?;
    Ok(TrainingDatasetRecord {
        id: DatasetId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        name: row.get(1).map_err(map_rusqlite)?,
        dataset_type: DatasetType::from_db_str(&dataset_type_raw)
            .ok_or_else(|| enum_err("dataset type", &dataset_type_raw))?,
        base_model_family: row.get(3).map_err(map_rusqlite)?,
        example_ids_hash: row.get(4).map_err(map_rusqlite)?,
        positive_count: count_from(row, 5)?,
        negative_count: count_from(row, 6)?,
        validation_count: count_from(row, 7)?,
        test_count: count_from(row, 8)?,
        privacy_level: ExportPrivacyLevel::from_db_str(&privacy_raw)
            .ok_or_else(|| enum_err("privacy level", &privacy_raw))?,
        export_format: ExportFormat::from_db_str(&format_raw)
            .ok_or_else(|| enum_err("export format", &format_raw))?,
        artifact_path: row.get(11).map_err(map_rusqlite)?,
        created_at: ts_from_db(&created_at)?,
    })
}

#[async_trait]
impl DatasetRepository for SqliteDatasetRepository {
    #[allow(clippy::cast_possible_wrap)]
    async fn append(&self, dataset: TrainingDatasetRecord) -> Result<DatasetId, StorageError> {
        let id = dataset.id.clone();
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO training_datasets (id, name, dataset_type, base_model_family, \
                 example_ids_hash, positive_count, negative_count, validation_count, \
                 test_count, privacy_level, export_format, artifact_path, created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    dataset.id.as_str(),
                    dataset.name,
                    dataset.dataset_type.as_str(),
                    dataset.base_model_family,
                    dataset.example_ids_hash,
                    dataset.positive_count as i64,
                    dataset.negative_count as i64,
                    dataset.validation_count as i64,
                    dataset.test_count as i64,
                    dataset.privacy_level.as_str(),
                    dataset.export_format.as_str(),
                    dataset.artifact_path,
                    ts_to_db(dataset.created_at),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn get(&self, id: &DatasetId) -> Result<Option<TrainingDatasetRecord>, StorageError> {
        let id = id.as_str().to_owned();
        self.backend.with_conn(|conn| {
            let sql = format!("SELECT {DATASET_COLUMNS} FROM training_datasets WHERE id = ?1");
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![id]).map_err(map_rusqlite)?;
            match rows.next().map_err(map_rusqlite)? {
                Some(row) => Ok(Some(row_to_dataset(row)?)),
                None => Ok(None),
            }
        })
    }

    async fn list(&self) -> Result<Vec<TrainingDatasetRecord>, StorageError> {
        self.backend.with_conn(|conn| {
            let sql = format!(
                "SELECT {DATASET_COLUMNS} FROM training_datasets ORDER BY created_at DESC, id DESC"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query([]).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_dataset(row)?);
            }
            Ok(out)
        })
    }
}

// ---- adapters --------------------------------------------------------------

/// SQLite implementation of [`AdapterRepository`].
pub struct SqliteAdapterRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteAdapterRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

fn row_to_adapter(row: &Row<'_>) -> Result<LoraAdapterRecord, StorageError> {
    let adapter_type_raw: String = row.get(2).map_err(map_rusqlite)?;
    let format_raw: String = row.get(3).map_err(map_rusqlite)?;
    let status_raw: String = row.get(11).map_err(map_rusqlite)?;
    let created_at: String = row.get(12).map_err(map_rusqlite)?;
    Ok(LoraAdapterRecord {
        id: AdapterId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        name: row.get(1).map_err(map_rusqlite)?,
        adapter_type: AdapterType::from_db_str(&adapter_type_raw)
            .ok_or_else(|| enum_err("adapter type", &adapter_type_raw))?,
        format: AdapterFormat::from_db_str(&format_raw)
            .ok_or_else(|| enum_err("adapter format", &format_raw))?,
        base_model_family: row.get(4).map_err(map_rusqlite)?,
        base_model_name: row.get(5).map_err(map_rusqlite)?,
        base_model_revision: row.get(6).map_err(map_rusqlite)?,
        tokenizer_hash: row.get(7).map_err(map_rusqlite)?,
        chat_template_hash: row.get(8).map_err(map_rusqlite)?,
        training_dataset_id: row
            .get::<_, Option<String>>(9)
            .map_err(map_rusqlite)?
            .map(DatasetId::from),
        artifact_path: row.get(10).map_err(map_rusqlite)?,
        status: AdapterStatus::from_db_str(&status_raw)
            .ok_or_else(|| enum_err("adapter status", &status_raw))?,
        created_at: ts_from_db(&created_at)?,
    })
}

#[async_trait]
impl AdapterRepository for SqliteAdapterRepository {
    async fn append(&self, adapter: LoraAdapterRecord) -> Result<AdapterId, StorageError> {
        let id = adapter.id.clone();
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO lora_adapters (id, name, adapter_type, format, base_model_family, \
                 base_model_name, base_model_revision, tokenizer_hash, chat_template_hash, \
                 training_dataset_id, artifact_path, status, created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    adapter.id.as_str(),
                    adapter.name,
                    adapter.adapter_type.as_str(),
                    adapter.format.as_str(),
                    adapter.base_model_family,
                    adapter.base_model_name,
                    adapter.base_model_revision,
                    adapter.tokenizer_hash,
                    adapter.chat_template_hash,
                    adapter.training_dataset_id.as_ref().map(DatasetId::as_str),
                    adapter.artifact_path,
                    adapter.status.as_str(),
                    ts_to_db(adapter.created_at),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn get(&self, id: &AdapterId) -> Result<Option<LoraAdapterRecord>, StorageError> {
        let id = id.as_str().to_owned();
        self.backend.with_conn(|conn| {
            let sql = format!("SELECT {ADAPTER_COLUMNS} FROM lora_adapters WHERE id = ?1");
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![id]).map_err(map_rusqlite)?;
            match rows.next().map_err(map_rusqlite)? {
                Some(row) => Ok(Some(row_to_adapter(row)?)),
                None => Ok(None),
            }
        })
    }

    async fn list(&self) -> Result<Vec<LoraAdapterRecord>, StorageError> {
        self.backend.with_conn(|conn| {
            let sql = format!(
                "SELECT {ADAPTER_COLUMNS} FROM lora_adapters ORDER BY created_at DESC, id DESC"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query([]).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_adapter(row)?);
            }
            Ok(out)
        })
    }

    async fn set_status(&self, id: &AdapterId, status: AdapterStatus) -> Result<(), StorageError> {
        let id = id.as_str().to_owned();
        self.backend.with_conn(|conn| {
            let affected = conn
                .execute(
                    "UPDATE lora_adapters SET status = ?2 WHERE id = ?1",
                    params![id, status.as_str()],
                )
                .map_err(map_rusqlite)?;
            if affected == 0 {
                return Err(StorageError::Constraint(format!("no adapter with id {id}")));
            }
            Ok(())
        })
    }
}

// ---- eval runs -------------------------------------------------------------

/// SQLite implementation of [`EvalRunRepository`].
pub struct SqliteEvalRunRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteEvalRunRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

fn row_to_eval_run(row: &Row<'_>) -> Result<LoraEvalRunRecord, StorageError> {
    let metrics_json: String = row.get(4).map_err(map_rusqlite)?;
    let approved: i64 = row.get(7).map_err(map_rusqlite)?;
    let created_at: String = row.get(8).map_err(map_rusqlite)?;
    let metrics: EvalMetrics = json_from_db(&metrics_json)?;
    Ok(LoraEvalRunRecord {
        id: EvalRunId::from(row.get::<_, String>(0).map_err(map_rusqlite)?),
        adapter_id: AdapterId::from(row.get::<_, String>(1).map_err(map_rusqlite)?),
        dataset_id: DatasetId::from(row.get::<_, String>(2).map_err(map_rusqlite)?),
        base_provider_id: row.get(3).map_err(map_rusqlite)?,
        metrics,
        safety_failures: count_from(row, 5)?,
        quality_score: row.get(6).map_err(map_rusqlite)?,
        approved_for_use: approved != 0,
        created_at: ts_from_db(&created_at)?,
    })
}

#[async_trait]
impl EvalRunRepository for SqliteEvalRunRepository {
    #[allow(clippy::cast_possible_wrap)]
    async fn append(&self, run: LoraEvalRunRecord) -> Result<EvalRunId, StorageError> {
        let id = run.id.clone();
        let metrics_json = json_to_db(&run.metrics)?;
        self.backend.with_conn(|conn| {
            conn.execute(
                "INSERT INTO lora_eval_runs (id, adapter_id, dataset_id, base_provider_id, \
                 metrics_json, safety_failures, quality_score, approved_for_use, created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    run.id.as_str(),
                    run.adapter_id.as_str(),
                    run.dataset_id.as_str(),
                    run.base_provider_id,
                    metrics_json,
                    run.safety_failures as i64,
                    run.quality_score,
                    i64::from(run.approved_for_use),
                    ts_to_db(run.created_at),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn list_for_adapter(
        &self,
        adapter_id: &AdapterId,
    ) -> Result<Vec<LoraEvalRunRecord>, StorageError> {
        let adapter_id = adapter_id.as_str().to_owned();
        self.backend.with_conn(|conn| {
            let sql = format!(
                "SELECT {EVAL_COLUMNS} FROM lora_eval_runs WHERE adapter_id = ?1 \
                 ORDER BY created_at DESC, id DESC"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_rusqlite)?;
            let mut rows = stmt.query(params![adapter_id]).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_eval_run(row)?);
            }
            Ok(out)
        })
    }
}
