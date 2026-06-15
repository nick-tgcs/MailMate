//! The training-layer storage ports: the dataset, adapter, and evaluation-run repositories.
//!
//! These are the only durable training tables — there is deliberately **no**
//! `training_examples` repository, because examples are derived on export from the per-task
//! feedback tables (the single source of truth) and never stored. An adapter is registered
//! as a `candidate` and reaches `active` only by a status transition the evaluation gate
//! drives, so promotion is always an explicit, audited write — never a side effect of
//! `append`.

use async_trait::async_trait;

use mailmate_common::error::StorageError;
use mailmate_common::ids::{AdapterId, DatasetId, EvalRunId};
use mailmate_common::training::{
    AdapterStatus, LoraAdapterRecord, LoraEvalRunRecord, TrainingDatasetRecord,
};

/// Persistence for derived training datasets (`training_datasets`).
#[async_trait]
pub trait DatasetRepository: Send + Sync {
    /// Append a dataset record. Returns its id.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn append(&self, dataset: TrainingDatasetRecord) -> Result<DatasetId, StorageError>;

    /// Fetch one dataset by id, or `None` if unknown.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn get(&self, id: &DatasetId) -> Result<Option<TrainingDatasetRecord>, StorageError>;

    /// Every dataset, newest first.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn list(&self) -> Result<Vec<TrainingDatasetRecord>, StorageError>;
}

/// Persistence for registered adapters (`lora_adapters`).
#[async_trait]
pub trait AdapterRepository: Send + Sync {
    /// Append an adapter record (always at a non-active status). Returns its id.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn append(&self, adapter: LoraAdapterRecord) -> Result<AdapterId, StorageError>;

    /// Fetch one adapter by id, or `None` if unknown.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn get(&self, id: &AdapterId) -> Result<Option<LoraAdapterRecord>, StorageError>;

    /// Every adapter, newest first.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn list(&self) -> Result<Vec<LoraAdapterRecord>, StorageError>;

    /// Transition an adapter's lifecycle status (the gate's promote/fail write).
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn set_status(&self, id: &AdapterId, status: AdapterStatus) -> Result<(), StorageError>;
}

/// Persistence for adapter evaluation runs (`lora_eval_runs`).
#[async_trait]
pub trait EvalRunRepository: Send + Sync {
    /// Append an evaluation-run record. Returns its id.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn append(&self, run: LoraEvalRunRecord) -> Result<EvalRunId, StorageError>;

    /// Every evaluation run for an adapter, newest first.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn list_for_adapter(
        &self,
        adapter_id: &AdapterId,
    ) -> Result<Vec<LoraEvalRunRecord>, StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_are_object_safe() {
        fn ds(_: &dyn DatasetRepository) {}
        fn ad(_: &dyn AdapterRepository) {}
        fn ev(_: &dyn EvalRunRepository) {}
        let _ = ds as fn(&dyn DatasetRepository);
        let _ = ad as fn(&dyn AdapterRepository);
        let _ = ev as fn(&dyn EvalRunRepository);
    }
}
