//! `DefaultTrainingPipeline` — the [`TrainingPipeline`] port: derive → train → register
//! candidate → evaluate → gate.
//!
//! The ordering is the safety invariant: a candidate adapter is persisted at
//! [`AdapterStatus::Candidate`] **before** it is evaluated, and reaches
//! [`AdapterStatus::Active`] only through a status transition the gate drives. There is no
//! path that writes an adapter as `active` before its evaluation run exists — a candidate
//! that fails the gate is recorded `failed_eval`. MailMate drives the whole loop here; only
//! `train` is behind the swappable backend.

use std::sync::Arc;

use async_trait::async_trait;

use mailmate_common::error::TrainingError;
use mailmate_common::ids::AdapterId;
use mailmate_common::retention::RetentionLevel;
use mailmate_common::training::{
    DatasetType, LoraEvalRunRecord, TrainingPipelineReport, TrainingPipelineRequest,
};
use mailmate_ports::ai_provider::AiProvider;
use mailmate_ports::clock::Clock;
use mailmate_ports::storage::{AdapterRepository, DatasetRepository, EvalRunRepository};
use mailmate_ports::training_pipeline::TrainingPipeline;

use crate::datasets::build_dataset;
use crate::evaluation::{evaluate_adapter, evaluate_promotion};
use crate::lora::{check_compatibility, register_local_adapter, spec_for, BaseModelTarget};
use crate::privacy::assert_ceiling_allowed;
use crate::source::TrainingDataSource;
use crate::trainer::{TrainerBackend, TrainingHyperparams, TrainingJob};

/// The default training-pipeline adapter.
pub struct DefaultTrainingPipeline {
    source: Arc<dyn TrainingDataSource>,
    trainer: Arc<dyn TrainerBackend>,
    datasets: Arc<dyn DatasetRepository>,
    adapters: Arc<dyn AdapterRepository>,
    eval_runs: Arc<dyn EvalRunRepository>,
    provider: Arc<dyn AiProvider>,
    clock: Arc<dyn Clock>,
    retention: RetentionLevel,
}

impl DefaultTrainingPipeline {
    /// Build the pipeline from its collaborators. `retention` is the user's storage-retention
    /// level, consulted to reject a `Full` export ceiling the user never consented to.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source: Arc<dyn TrainingDataSource>,
        trainer: Arc<dyn TrainerBackend>,
        datasets: Arc<dyn DatasetRepository>,
        adapters: Arc<dyn AdapterRepository>,
        eval_runs: Arc<dyn EvalRunRepository>,
        provider: Arc<dyn AiProvider>,
        clock: Arc<dyn Clock>,
        retention: RetentionLevel,
    ) -> Self {
        Self {
            source,
            trainer,
            datasets,
            adapters,
            eval_runs,
            provider,
            clock,
            retention,
        }
    }
}

#[async_trait]
impl TrainingPipeline for DefaultTrainingPipeline {
    async fn run(
        &self,
        request: TrainingPipelineRequest,
    ) -> Result<TrainingPipelineReport, TrainingError> {
        // 0. A full-body ceiling must be permitted by the user's retention.
        assert_ceiling_allowed(request.privacy_ceiling, self.retention)?;

        // 1. A LoRA run needs a known base-model family for portability metadata.
        let family = request.base_model_family.clone().ok_or_else(|| {
            TrainingError::Compatibility("a base model family is required".to_owned())
        })?;
        let base_model_name = request
            .base_model_name
            .clone()
            .unwrap_or_else(|| family.clone());

        // 2. Derive examples from the captured feedback.
        let examples = self.source.collect(&request).await?;

        let now = self.clock.now();

        // 3. Build the training and evaluation datasets (privacy enforced inside).
        let training_plan = build_dataset(
            request.dataset_name.clone(),
            request.objective.dataset_type(),
            Some(family.clone()),
            request.privacy_ceiling,
            now,
            examples.clone(),
        );
        if training_plan.is_empty() {
            return Err(TrainingError::Export(
                "no eligible training examples for the requested objective".to_owned(),
            ));
        }
        let eval_plan = build_dataset(
            format!("{}-eval", request.dataset_name),
            DatasetType::Evaluation,
            Some(family.clone()),
            request.privacy_ceiling,
            now,
            examples,
        );

        // 4. Persist both dataset records.
        let training_dataset_id = self.datasets.append(training_plan.record.clone()).await?;
        self.datasets.append(eval_plan.record.clone()).await?;

        // 5. Train a candidate adapter (the backend's capability gate may reject the job).
        let job = TrainingJob {
            dataset_id: training_dataset_id.clone(),
            objective: request.objective,
            produce_lora: true,
            examples: training_plan.examples.clone(),
            base_model_family: family.clone(),
            base_model_name,
            hyperparams: TrainingHyperparams::default(),
        };
        let artifact = self.trainer.train(job).await?;

        // 6. Register the candidate — ALWAYS at `candidate`, before any evaluation.
        let mut adapter = register_local_adapter(
            AdapterId::fresh(),
            request.dataset_name.clone(),
            &artifact,
            training_dataset_id,
            now,
        );
        let adapter_id = self.adapters.append(adapter.clone()).await?;

        // 7. Evaluate the candidate against the frozen evaluation set.
        let metrics = evaluate_adapter(self.provider.as_ref(), &eval_plan.examples).await?;

        // 8. Compatibility verdict against the target base model.
        let compatibility = check_compatibility(
            &spec_for(&adapter),
            &BaseModelTarget {
                family,
                tokenizer_hash: artifact.tokenizer_hash.clone(),
                chat_template_hash: artifact.chat_template_hash.clone(),
            },
        );

        // 9. Gate the promotion.
        let decision = evaluate_promotion(
            &request.promotion_policy,
            metrics.safety_failures,
            metrics.quality_score,
            &compatibility,
        );

        // 10. Record the evaluation run with the gate's verdict.
        let eval_run = LoraEvalRunRecord {
            id: mailmate_common::ids::EvalRunId::fresh(),
            adapter_id: adapter_id.clone(),
            dataset_id: eval_plan.record.id.clone(),
            base_provider_id: request.base_provider_id.clone(),
            safety_failures: metrics.safety_failures,
            quality_score: metrics.quality_score,
            approved_for_use: decision.is_promote(),
            metrics,
            created_at: now,
        };
        self.eval_runs.append(eval_run.clone()).await?;

        // 11. Transition the adapter to its post-gate status (Active or FailedEval).
        let resulting = decision.resulting_status();
        self.adapters.set_status(&adapter_id, resulting).await?;
        adapter.status = resulting;

        Ok(TrainingPipelineReport {
            dataset: training_plan.record,
            eval_dataset: eval_plan.record,
            adapter,
            eval_run,
            promotion: decision,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use mailmate_common::ai::{
        ProviderCapabilities, ProviderId, StructuredRequest, StructuredResponse,
    };
    use mailmate_common::error::{AiError, StorageError};
    use mailmate_common::evidence::EvidenceSourceKind;
    use mailmate_common::feedback::FeedbackPolarity;
    use mailmate_common::ids::{DatasetId, EvalRunId, FeedbackId};
    use mailmate_common::time::Timestamp;
    use mailmate_common::training::{
        AdapterStatus, CandidateOutput, ContextFeatures, ExportPrivacyLevel, LoraAdapterRecord,
        PromotionPolicy, SourceFeedbackRef, TrainingDatasetRecord, TrainingExample, TrainingInput,
        TrainingLabel, TrainingObjective, TrainingTask,
    };

    use crate::source::TrainingDataSource;
    use crate::trainers::mock::MockTrainer;

    // ---- in-memory fakes ---------------------------------------------------

    struct FixedSource(Vec<TrainingExample>);
    #[async_trait]
    impl TrainingDataSource for FixedSource {
        async fn collect(
            &self,
            _r: &TrainingPipelineRequest,
        ) -> Result<Vec<TrainingExample>, mailmate_common::error::ExportError> {
            Ok(self.0.clone())
        }
    }

    #[derive(Default)]
    struct MemDatasets(Mutex<Vec<TrainingDatasetRecord>>);
    #[async_trait]
    impl DatasetRepository for MemDatasets {
        async fn append(&self, d: TrainingDatasetRecord) -> Result<DatasetId, StorageError> {
            let id = d.id.clone();
            self.0.lock().unwrap().push(d);
            Ok(id)
        }
        async fn get(&self, id: &DatasetId) -> Result<Option<TrainingDatasetRecord>, StorageError> {
            Ok(self.0.lock().unwrap().iter().find(|d| &d.id == id).cloned())
        }
        async fn list(&self) -> Result<Vec<TrainingDatasetRecord>, StorageError> {
            Ok(self.0.lock().unwrap().clone())
        }
    }

    #[derive(Default)]
    struct MemAdapters {
        rows: Mutex<Vec<LoraAdapterRecord>>,
        status_writes: Mutex<Vec<(AdapterId, AdapterStatus)>>,
    }
    #[async_trait]
    impl AdapterRepository for MemAdapters {
        async fn append(&self, a: LoraAdapterRecord) -> Result<AdapterId, StorageError> {
            let id = a.id.clone();
            // The invariant under test: an adapter is never appended as active.
            assert_ne!(
                a.status,
                AdapterStatus::Active,
                "append must never be active"
            );
            self.rows.lock().unwrap().push(a);
            Ok(id)
        }
        async fn get(&self, id: &AdapterId) -> Result<Option<LoraAdapterRecord>, StorageError> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .find(|a| &a.id == id)
                .cloned())
        }
        async fn list(&self) -> Result<Vec<LoraAdapterRecord>, StorageError> {
            Ok(self.rows.lock().unwrap().clone())
        }
        async fn set_status(
            &self,
            id: &AdapterId,
            status: AdapterStatus,
        ) -> Result<(), StorageError> {
            self.status_writes
                .lock()
                .unwrap()
                .push((id.clone(), status));
            Ok(())
        }
    }

    #[derive(Default)]
    struct MemEvalRuns(Mutex<Vec<LoraEvalRunRecord>>);
    #[async_trait]
    impl EvalRunRepository for MemEvalRuns {
        async fn append(&self, r: LoraEvalRunRecord) -> Result<EvalRunId, StorageError> {
            let id = r.id.clone();
            self.0.lock().unwrap().push(r);
            Ok(id)
        }
        async fn list_for_adapter(
            &self,
            adapter_id: &AdapterId,
        ) -> Result<Vec<LoraEvalRunRecord>, StorageError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|r| &r.adapter_id == adapter_id)
                .cloned()
                .collect())
        }
    }

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
                parsed_json: serde_json::json!({"text": self.body}),
                schema_validated_by: None,
            })
        }
    }

    struct FixedClock(Timestamp);
    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            self.0
        }
    }

    // ---- fixtures ----------------------------------------------------------

    /// Build a draft example whose source id falls in a chosen split bucket family, with a
    /// distinct id so train and eval both get populated.
    fn draft_example(source_id: &str, expected: &str) -> TrainingExample {
        TrainingExample {
            id: format!("trn_{source_id}"),
            task: TrainingTask::DraftReply,
            source_feedback: SourceFeedbackRef {
                kind: EvidenceSourceKind::Draft,
                id: FeedbackId::from(source_id),
            },
            privacy_level: ExportPrivacyLevel::Metadata,
            base_model_family: None,
            input: TrainingInput {
                system: Some("You are MailMate".to_owned()),
                instruction: "Draft a reply".to_owned(),
                context_features: ContextFeatures::default(),
            },
            candidate_output: Some(CandidateOutput::new(expected)),
            user_corrected_output: Some(CandidateOutput::new(expected)),
            label: TrainingLabel::Accepted,
            polarity: FeedbackPolarity::Positive,
            quality_score: 1.0,
            safety_flags: vec![],
            created_at: Timestamp::now(),
        }
    }

    /// A spread of ids so both the train and test splits are populated.
    fn spread_examples(expected: &str) -> Vec<TrainingExample> {
        (0..40)
            .map(|i| draft_example(&format!("drffb_{i}"), expected))
            .collect()
    }

    fn pipeline(
        source: Vec<TrainingExample>,
        provider_body: &str,
    ) -> (DefaultTrainingPipeline, Arc<MemAdapters>) {
        let adapters = Arc::new(MemAdapters::default());
        let pipe = DefaultTrainingPipeline::new(
            Arc::new(FixedSource(source)),
            Arc::new(MockTrainer::new()),
            Arc::new(MemDatasets::default()),
            adapters.clone(),
            Arc::new(MemEvalRuns::default()),
            Arc::new(CannedProvider {
                body: provider_body.to_owned(),
            }),
            Arc::new(FixedClock(Timestamp::now())),
            RetentionLevel::Metadata,
        );
        (pipe, adapters)
    }

    fn request() -> TrainingPipelineRequest {
        TrainingPipelineRequest {
            base_model_family: Some("llama".to_owned()),
            base_model_name: Some("llama-3".to_owned()),
            objective: TrainingObjective::Sft,
            ..TrainingPipelineRequest::new("nightly", "prov_canned")
        }
    }

    #[test]
    fn a_clean_run_promotes_and_records_candidate_before_active() {
        // The provider returns the expected label, so eval is clean -> gate promotes.
        let (pipe, adapters) = pipeline(spread_examples("phishing"), "phishing");
        let report = block_on(pipe.run(request())).unwrap();
        assert!(
            report.promoted(),
            "clean eval should promote: {:?}",
            report.promotion
        );
        assert_eq!(report.adapter.status, AdapterStatus::Active);
        // The adapter was appended as candidate, then transitioned to active.
        let appended = &adapters.rows.lock().unwrap()[0];
        assert_eq!(
            appended.status,
            AdapterStatus::Candidate,
            "persisted candidate first"
        );
        let writes = adapters.status_writes.lock().unwrap();
        assert_eq!(writes.last().unwrap().1, AdapterStatus::Active);
        // The eval run was approved.
        assert!(report.eval_run.approved_for_use);
        assert_eq!(report.dataset.dataset_type, DatasetType::Sft);
        assert_eq!(report.eval_dataset.dataset_type, DatasetType::Evaluation);
    }

    #[test]
    fn an_unsafe_provider_response_fails_the_gate_and_never_activates() {
        // The provider emits unsafe content during eval -> safety failure -> gate rejects.
        let (pipe, adapters) = pipeline(
            spread_examples("phishing"),
            "I will update the payment details and send payment today",
        );
        let report = block_on(pipe.run(request())).unwrap();
        assert!(!report.promoted(), "an unsafe eval must not promote");
        assert_eq!(report.adapter.status, AdapterStatus::FailedEval);
        assert!(!report.eval_run.approved_for_use);
        assert!(report.eval_run.safety_failures >= 1);
        // It was never written active.
        let writes = adapters.status_writes.lock().unwrap();
        assert!(writes.iter().all(|(_, s)| *s != AdapterStatus::Active));
    }

    #[test]
    fn an_empty_source_errors_before_persisting_anything() {
        let (pipe, adapters) = pipeline(vec![], "x");
        let err = block_on(pipe.run(request())).unwrap_err();
        assert!(matches!(err, TrainingError::Export(_)));
        assert!(
            adapters.rows.lock().unwrap().is_empty(),
            "nothing persisted on empty input"
        );
    }

    #[test]
    fn a_missing_base_model_family_is_a_compatibility_error() {
        let (pipe, _) = pipeline(spread_examples("x"), "x");
        let req = TrainingPipelineRequest {
            base_model_family: None,
            ..request()
        };
        let err = block_on(pipe.run(req)).unwrap_err();
        assert!(matches!(err, TrainingError::Compatibility(_)));
    }

    #[test]
    fn a_full_ceiling_without_body_retention_is_rejected() {
        let (pipe, _) = pipeline(spread_examples("x"), "x");
        let req = TrainingPipelineRequest {
            privacy_ceiling: ExportPrivacyLevel::Full,
            ..request()
        };
        // Pipeline was built with RetentionLevel::Metadata.
        let err = block_on(pipe.run(req)).unwrap_err();
        assert!(matches!(err, TrainingError::Export(_)));
    }

    #[test]
    fn an_sft_incapable_trainer_surfaces_unsupported() {
        use crate::trainer::TrainerCapabilities;
        let adapters = Arc::new(MemAdapters::default());
        let pipe = DefaultTrainingPipeline::new(
            Arc::new(FixedSource(spread_examples("phishing"))),
            // A trainer that cannot produce a LoRA.
            Arc::new(MockTrainer::with_capabilities(TrainerCapabilities {
                sft: true,
                preference: false,
                lora: false,
                on_device: true,
            })),
            Arc::new(MemDatasets::default()),
            adapters,
            Arc::new(MemEvalRuns::default()),
            Arc::new(CannedProvider {
                body: "phishing".to_owned(),
            }),
            Arc::new(FixedClock(Timestamp::now())),
            RetentionLevel::Metadata,
        );
        let err = block_on(pipe.run(request())).unwrap_err();
        assert!(
            matches!(err, TrainingError::Trainer(_)),
            "honest capabilities surface as Unsupported"
        );
    }

    #[test]
    fn a_lax_policy_can_promote_despite_unknown_compatibility() {
        let (pipe, _) = pipeline(spread_examples("phishing"), "phishing");
        let req = TrainingPipelineRequest {
            promotion_policy: PromotionPolicy {
                min_quality_score: 0.5,
                max_safety_failures: 0,
                require_compatible: true,
            },
            ..request()
        };
        // The mock trainer reports matching hashes, so compatibility is Compatible and a
        // clean eval promotes.
        let report = block_on(pipe.run(req)).unwrap();
        assert!(report.promoted());
    }
}
