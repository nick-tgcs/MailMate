//! End-to-end integration of the training pipeline over the REAL SQLite repositories: seed
//! the per-task feedback tables, then run `DefaultTrainingPipeline` with a `MockTrainer` and
//! a canned provider and assert the whole loop — derive examples from feedback → build &
//! persist datasets → train a candidate → register it as `candidate` → evaluate → gate →
//! promote. No GPU and no external tool are involved. This proves "derive on export" (item 3)
//! and the safety ordering (candidate persisted before any activation) against real storage.

use std::sync::Arc;

use async_trait::async_trait;
use futures::executor::block_on;

use mailmate_common::ai::{
    ProviderCapabilities, ProviderId, StructuredRequest, StructuredResponse,
};
use mailmate_common::error::AiError;
use mailmate_common::features::FeatureVector;
use mailmate_common::feedback::{
    ClassificationFeedback, ClassificationFeedbackRow, FeedbackPolarity, FilingFeedback,
    PinnedVersions, RuleProposalFeedback,
};
use mailmate_common::ids::MessageId;
use mailmate_common::retention::RetentionLevel;
use mailmate_common::time::Timestamp;
use mailmate_common::training::{
    AdapterStatus, ExportPrivacyLevel, TrainingObjective, TrainingPipelineRequest, TrainingTask,
};
use mailmate_ports::ai_provider::AiProvider;
use mailmate_ports::storage::FeedbackRepository;
use mailmate_ports::training_pipeline::TrainingPipeline;
use mailmate_storage::{
    open_and_migrate, SqliteAdapterRepository, SqliteDatasetRepository, SqliteEvalRunRepository,
    SqliteFeedbackRepository, StorageConfig,
};
use mailmate_test_support::fakes::FakeClock;
use mailmate_training::pipeline::DefaultTrainingPipeline;
use mailmate_training::source::FeedbackTrainingSource;
use mailmate_training::trainers::mock::MockTrainer;

/// A provider that returns a fixed body for every request (so the eval is deterministic).
struct CannedProvider {
    body: String,
}

#[async_trait]
impl AiProvider for CannedProvider {
    fn id(&self) -> ProviderId {
        ProviderId::from("prov_canned")
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
    async fn complete_structured(
        &self,
        _request: StructuredRequest,
    ) -> Result<StructuredResponse, AiError> {
        Ok(StructuredResponse {
            raw_text: self.body.clone(),
            parsed_json: serde_json::json!({"label": self.body}),
            schema_validated_by: None,
        })
    }
}

fn classification_row(
    i: usize,
    label: &str,
    polarity: FeedbackPolarity,
) -> ClassificationFeedbackRow {
    ClassificationFeedbackRow {
        id: ClassificationFeedback::fresh_id(),
        message_id: MessageId::from(format!("msg_{i}")),
        pinned_versions: PinnedVersions::default(),
        ai_label: Some(label.to_owned()),
        ai_score: Some(0.9),
        ai_rationale: None,
        human_label: label.to_owned(),
        human_reason_code: Some("known_sender".to_owned()),
        human_reason_text: None,
        salient_features: FeatureVector::new(),
        polarity,
        created_at: Timestamp::now(),
    }
}

fn pipeline_over(
    backend: Arc<mailmate_storage::SqliteBackend>,
    provider_body: &str,
) -> DefaultTrainingPipeline {
    let feedback = Arc::new(SqliteFeedbackRepository::new(backend.clone()));
    let source = FeedbackTrainingSource::new(
        feedback.clone() as Arc<dyn FeedbackRepository<ClassificationFeedback>>,
        feedback.clone() as Arc<dyn FeedbackRepository<FilingFeedback>>,
        feedback as Arc<dyn FeedbackRepository<RuleProposalFeedback>>,
    );
    DefaultTrainingPipeline::new(
        Arc::new(source),
        Arc::new(MockTrainer::new()),
        Arc::new(SqliteDatasetRepository::new(backend.clone())),
        Arc::new(SqliteAdapterRepository::new(backend.clone())),
        Arc::new(SqliteEvalRunRepository::new(backend)),
        Arc::new(CannedProvider {
            body: provider_body.to_owned(),
        }),
        Arc::new(FakeClock::new(Timestamp::now())),
        RetentionLevel::Metadata,
    )
}

fn request() -> TrainingPipelineRequest {
    TrainingPipelineRequest {
        tasks: vec![TrainingTask::Classification],
        base_model_family: Some("llama".to_owned()),
        base_model_name: Some("llama-3".to_owned()),
        objective: TrainingObjective::Sft,
        privacy_ceiling: ExportPrivacyLevel::Metadata,
        ..TrainingPipelineRequest::new("nightly", "prov_canned")
    }
}

#[test]
fn full_pipeline_derives_from_feedback_trains_and_promotes_a_clean_adapter() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    // Seed 50 positive "phishing" classification corrections across messages.
    let feedback = SqliteFeedbackRepository::new(backend.clone());
    for i in 0..50 {
        block_on(FeedbackRepository::<ClassificationFeedback>::append(
            &feedback,
            classification_row(i, "phishing", FeedbackPolarity::Positive),
        ))
        .unwrap();
    }

    // The provider returns the expected label -> a clean evaluation -> the gate promotes.
    let pipeline = pipeline_over(backend.clone(), "phishing");
    let report = block_on(pipeline.run(request())).unwrap();

    assert!(
        report.promoted(),
        "a clean eval over real storage promotes: {:?}",
        report.promotion
    );
    assert_eq!(report.adapter.status, AdapterStatus::Active);

    // The datasets and the eval run were persisted.
    let datasets = SqliteDatasetRepository::new(backend.clone());
    let stored_datasets =
        block_on(mailmate_ports::storage::DatasetRepository::list(&datasets)).unwrap();
    assert_eq!(stored_datasets.len(), 2, "train + eval datasets persisted");

    let adapters = SqliteAdapterRepository::new(backend.clone());
    let stored = block_on(mailmate_ports::storage::AdapterRepository::get(
        &adapters,
        &report.adapter.id,
    ))
    .unwrap()
    .unwrap();
    assert_eq!(
        stored.status,
        AdapterStatus::Active,
        "the persisted adapter was promoted"
    );

    let eval_runs = SqliteEvalRunRepository::new(backend);
    let runs = block_on(
        mailmate_ports::storage::EvalRunRepository::list_for_adapter(
            &eval_runs,
            &report.adapter.id,
        ),
    )
    .unwrap();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].approved_for_use);
}

#[test]
fn full_pipeline_records_failed_eval_without_activating_on_unsafe_output() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let feedback = SqliteFeedbackRepository::new(backend.clone());
    for i in 0..50 {
        block_on(FeedbackRepository::<ClassificationFeedback>::append(
            &feedback,
            classification_row(i, "phishing", FeedbackPolarity::Positive),
        ))
        .unwrap();
    }

    // The provider emits unsafe content during eval -> safety failure -> gate rejects.
    let pipeline = pipeline_over(
        backend.clone(),
        "I will update the payment details and send payment today",
    );
    let report = block_on(pipeline.run(request())).unwrap();

    assert!(!report.promoted());
    assert_eq!(report.adapter.status, AdapterStatus::FailedEval);

    // The persisted adapter is failed_eval, NEVER active.
    let adapters = SqliteAdapterRepository::new(backend);
    let stored = block_on(mailmate_ports::storage::AdapterRepository::get(
        &adapters,
        &report.adapter.id,
    ))
    .unwrap()
    .unwrap();
    assert_eq!(stored.status, AdapterStatus::FailedEval);
}

#[test]
fn an_empty_feedback_corpus_errors_and_persists_nothing() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let pipeline = pipeline_over(backend.clone(), "phishing");
    let err = block_on(pipeline.run(request())).unwrap_err();
    assert!(matches!(
        err,
        mailmate_common::error::TrainingError::Export(_)
    ));

    let adapters = SqliteAdapterRepository::new(backend);
    assert!(
        block_on(mailmate_ports::storage::AdapterRepository::list(&adapters))
            .unwrap()
            .is_empty()
    );
}
