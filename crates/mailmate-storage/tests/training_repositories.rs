//! Integration tests for the Phase-9 training repositories — datasets, adapters, and
//! evaluation runs — driven through the public adapters over a migrated in-memory backend.
//! These prove the durable surface of the training layer: an adapter is inserted at a
//! non-active status and only `set_status` transitions it, and an eval run round-trips its
//! metrics JSON and the gate's `approved_for_use` verdict.

use std::sync::Arc;

use futures::executor::block_on;

use mailmate_common::ids::{AdapterId, DatasetId, EvalRunId};
use mailmate_common::time::Timestamp;
use mailmate_common::training::{
    AdapterFormat, AdapterStatus, AdapterType, DatasetType, EvalMetrics, ExportFormat,
    ExportPrivacyLevel, LoraAdapterRecord, LoraEvalRunRecord, TrainingDatasetRecord,
};
use mailmate_ports::storage::{AdapterRepository, DatasetRepository, EvalRunRepository};
use mailmate_storage::{
    open_and_migrate, SqliteAdapterRepository, SqliteBackend, SqliteDatasetRepository,
    SqliteEvalRunRepository, StorageConfig,
};

fn backend() -> Arc<SqliteBackend> {
    open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap()
}

fn dataset(id: &str, dataset_type: DatasetType) -> TrainingDatasetRecord {
    TrainingDatasetRecord {
        id: DatasetId::from(id),
        name: "nightly".to_owned(),
        dataset_type,
        base_model_family: Some("llama".to_owned()),
        example_ids_hash: "abc123".to_owned(),
        positive_count: 12,
        negative_count: 3,
        validation_count: 2,
        test_count: 4,
        privacy_level: ExportPrivacyLevel::Redacted,
        export_format: ExportFormat::JsonlChat,
        artifact_path: None,
        created_at: Timestamp::now(),
    }
}

fn adapter(id: &str, dataset_id: Option<&str>, status: AdapterStatus) -> LoraAdapterRecord {
    LoraAdapterRecord {
        id: AdapterId::from(id),
        name: "draft-style-v1".to_owned(),
        adapter_type: AdapterType::Lora,
        format: AdapterFormat::Safetensors,
        base_model_family: "llama".to_owned(),
        base_model_name: "llama-3".to_owned(),
        base_model_revision: Some("rev1".to_owned()),
        tokenizer_hash: Some("tok_a".to_owned()),
        chat_template_hash: None,
        training_dataset_id: dataset_id.map(DatasetId::from),
        artifact_path: "/adapters/x.safetensors".to_owned(),
        status,
        created_at: Timestamp::now(),
    }
}

#[test]
fn dataset_round_trips_through_append_get_and_list() {
    let backend = backend();
    let repo = SqliteDatasetRepository::new(backend);
    let record = dataset("ds_1", DatasetType::Sft);
    let id = block_on(repo.append(record.clone())).unwrap();
    assert_eq!(id, DatasetId::from("ds_1"));

    let got = block_on(repo.get(&id)).unwrap().unwrap();
    assert_eq!(got, record, "the dataset round-trips field for field");
    assert!(block_on(repo.get(&DatasetId::from("ds_missing")))
        .unwrap()
        .is_none());

    block_on(repo.append(dataset("ds_2", DatasetType::Evaluation))).unwrap();
    assert_eq!(block_on(repo.list()).unwrap().len(), 2);
}

#[test]
fn adapter_is_inserted_non_active_and_only_set_status_promotes_it() {
    let backend = backend();
    let datasets = SqliteDatasetRepository::new(backend.clone());
    block_on(datasets.append(dataset("ds_1", DatasetType::Sft))).unwrap();
    let repo = SqliteAdapterRepository::new(backend);

    let candidate = adapter("lora_1", Some("ds_1"), AdapterStatus::Candidate);
    let id = block_on(repo.append(candidate.clone())).unwrap();
    let got = block_on(repo.get(&id)).unwrap().unwrap();
    assert_eq!(got, candidate);
    assert_eq!(
        got.status,
        AdapterStatus::Candidate,
        "registered as candidate"
    );

    // The gate's promote write.
    block_on(repo.set_status(&id, AdapterStatus::Active)).unwrap();
    assert_eq!(
        block_on(repo.get(&id)).unwrap().unwrap().status,
        AdapterStatus::Active
    );

    // Setting the status of an unknown adapter is a constraint error.
    let err = block_on(repo.set_status(&AdapterId::from("lora_missing"), AdapterStatus::Retired))
        .unwrap_err();
    assert!(matches!(
        err,
        mailmate_common::error::StorageError::Constraint(_)
    ));
}

#[test]
fn adapter_with_a_missing_dataset_fk_is_rejected() {
    let backend = backend();
    let repo = SqliteAdapterRepository::new(backend);
    // training_dataset_id references a non-existent dataset -> FK violation (pragma is ON).
    let err = block_on(repo.append(adapter("lora_x", Some("ds_nope"), AdapterStatus::Candidate)))
        .unwrap_err();
    assert!(matches!(
        err,
        mailmate_common::error::StorageError::Constraint(_)
    ));
}

#[test]
fn adapter_without_a_dataset_can_be_imported() {
    // An externally-imported adapter has no local dataset (nullable FK).
    let backend = backend();
    let repo = SqliteAdapterRepository::new(backend);
    let id = block_on(repo.append(adapter("lora_ext", None, AdapterStatus::Candidate))).unwrap();
    let got = block_on(repo.get(&id)).unwrap().unwrap();
    assert!(got.training_dataset_id.is_none());
}

#[test]
fn eval_run_round_trips_metrics_and_the_gate_verdict() {
    let backend = backend();
    let datasets = SqliteDatasetRepository::new(backend.clone());
    let adapters = SqliteAdapterRepository::new(backend.clone());
    block_on(datasets.append(dataset("ds_1", DatasetType::Evaluation))).unwrap();
    block_on(adapters.append(adapter("lora_1", Some("ds_1"), AdapterStatus::Candidate))).unwrap();
    let repo = SqliteEvalRunRepository::new(backend);

    let mut metrics = EvalMetrics {
        accuracy: Some(0.92),
        win_rate: Some(0.6),
        safety_failures: 0,
        quality_score: 0.88,
        ..EvalMetrics::default()
    };
    metrics.extra.insert("f1".to_owned(), 0.9);
    let run = LoraEvalRunRecord {
        id: EvalRunId::from("eval_1"),
        adapter_id: AdapterId::from("lora_1"),
        dataset_id: DatasetId::from("ds_1"),
        base_provider_id: "prov_mock".to_owned(),
        metrics: metrics.clone(),
        safety_failures: 0,
        quality_score: 0.88,
        approved_for_use: true,
        created_at: Timestamp::now(),
    };
    block_on(repo.append(run.clone())).unwrap();

    let runs = block_on(repo.list_for_adapter(&AdapterId::from("lora_1"))).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0], run,
        "the eval run round-trips, metrics JSON included"
    );
    assert!(runs[0].approved_for_use);
    assert_eq!(runs[0].metrics.extra.get("f1"), Some(&0.9));

    // No runs for a different adapter.
    assert!(
        block_on(repo.list_for_adapter(&AdapterId::from("lora_other")))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn eval_run_with_a_missing_adapter_fk_is_rejected() {
    let backend = backend();
    let datasets = SqliteDatasetRepository::new(backend.clone());
    block_on(datasets.append(dataset("ds_1", DatasetType::Evaluation))).unwrap();
    let repo = SqliteEvalRunRepository::new(backend);
    let run = LoraEvalRunRecord {
        id: EvalRunId::from("eval_x"),
        adapter_id: AdapterId::from("lora_nope"),
        dataset_id: DatasetId::from("ds_1"),
        base_provider_id: "p".to_owned(),
        metrics: EvalMetrics::default(),
        safety_failures: 0,
        quality_score: 0.0,
        approved_for_use: false,
        created_at: Timestamp::now(),
    };
    let err = block_on(repo.append(run)).unwrap_err();
    assert!(matches!(
        err,
        mailmate_common::error::StorageError::Constraint(_)
    ));
}
