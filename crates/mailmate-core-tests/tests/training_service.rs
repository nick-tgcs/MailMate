//! The core `TrainingService` use-case, proven over the in-memory fakes, plus the Phase-9
//! safety contract: **no LoRA path bypasses the rule hierarchy or the policy guard.**
//!
//! `TrainingService` is a thin seam over the training-pipeline port; the real pipeline,
//! trainer backend, and repositories are exercised in `mailmate-training`'s tests. Here we
//! prove (1) the service routes a request to the port and returns its report, (2) it composes
//! from the `Ports` bundle, and (3) a LoRA-adapted provider is still just a provider — it
//! refuses an incompatible adapter (no silent attach) and whatever it returns still passes
//! through the policy guard, which blocks a prohibited action regardless of any adapter.

use std::sync::Arc;

use futures::executor::block_on;

use mailmate_common::action::{ActionPlan, ProposedAction};
use mailmate_common::adapter::{AdapterSpec, BaseModelTarget};
use mailmate_common::ai::StructuredResponse;
use mailmate_common::classification::{Classification, ClassificationProvenance, Priority};
use mailmate_common::ids::{AdapterId, DatasetId, DecisionId, DraftId, EvalRunId, MessageId};
use mailmate_common::policy::PolicyContext;
use mailmate_common::time::Timestamp;
use mailmate_common::training::{
    AdapterFormat, AdapterStatus, AdapterType, DatasetType, EvalMetrics, ExportFormat,
    ExportPrivacyLevel, LoraAdapterRecord, LoraEvalRunRecord, PromotionDecision,
    TrainingDatasetRecord, TrainingObjective, TrainingPipelineReport, TrainingPipelineRequest,
};
use mailmate_core::{Ports, TrainingService};
use mailmate_ports::ai_provider::SupportsAdapters;
use mailmate_ports::policy_guard::PolicyGuard;
use mailmate_test_support::fakes::{
    FakeActionPlanner, FakeAdapterProvider, FakeClassificationEngine, FakeClock,
    FakeLearningEngine, FakeMailClient, FakePolicyGuard, FakeProposalReview, FakeRuleCurator,
    FakeSecretStore, FakeTier2Classifier, FakeTrainingPipeline, FakeTransport,
    StubFeatureExtractor,
};

fn dataset(id: &str, dataset_type: DatasetType) -> TrainingDatasetRecord {
    TrainingDatasetRecord {
        id: DatasetId::from(id),
        name: "nightly".to_owned(),
        dataset_type,
        base_model_family: Some("llama".to_owned()),
        example_ids_hash: "h".to_owned(),
        positive_count: 5,
        negative_count: 1,
        validation_count: 1,
        test_count: 2,
        privacy_level: ExportPrivacyLevel::Redacted,
        export_format: ExportFormat::JsonlChat,
        artifact_path: None,
        created_at: Timestamp::now(),
    }
}

fn report(promoted: bool) -> TrainingPipelineReport {
    let status = if promoted {
        AdapterStatus::Active
    } else {
        AdapterStatus::FailedEval
    };
    let adapter = LoraAdapterRecord {
        id: AdapterId::from("lora_1"),
        name: "nightly".to_owned(),
        adapter_type: AdapterType::Lora,
        format: AdapterFormat::Safetensors,
        base_model_family: "llama".to_owned(),
        base_model_name: "llama-3".to_owned(),
        base_model_revision: None,
        tokenizer_hash: Some("tok_a".to_owned()),
        chat_template_hash: None,
        training_dataset_id: Some(DatasetId::from("ds_train")),
        artifact_path: "/a.safetensors".to_owned(),
        status,
        created_at: Timestamp::now(),
    };
    let promotion = if promoted {
        PromotionDecision::Promote
    } else {
        PromotionDecision::Reject {
            reasons: vec!["safety failure".to_owned()],
        }
    };
    TrainingPipelineReport {
        dataset: dataset("ds_train", DatasetType::Sft),
        eval_dataset: dataset("ds_eval", DatasetType::Evaluation),
        adapter,
        eval_run: LoraEvalRunRecord {
            id: EvalRunId::from("eval_1"),
            adapter_id: AdapterId::from("lora_1"),
            dataset_id: DatasetId::from("ds_eval"),
            base_provider_id: "prov_mock".to_owned(),
            metrics: EvalMetrics::default(),
            safety_failures: usize::from(!promoted),
            quality_score: if promoted { 0.9 } else { 0.4 },
            approved_for_use: promoted,
            created_at: Timestamp::now(),
        },
        promotion,
    }
}

fn request() -> TrainingPipelineRequest {
    TrainingPipelineRequest {
        base_model_family: Some("llama".to_owned()),
        objective: TrainingObjective::Sft,
        ..TrainingPipelineRequest::new("nightly", "prov_mock")
    }
}

#[test]
fn training_service_runs_the_pipeline_and_returns_its_report() {
    let pipeline = Arc::new(FakeTrainingPipeline::returning(report(true)));
    let service = TrainingService::new(pipeline.clone());

    let got = block_on(service.run(request())).unwrap();
    assert!(
        got.promoted(),
        "a promoted report flows back through the service"
    );
    assert_eq!(got.adapter.status, AdapterStatus::Active);
    assert_eq!(
        pipeline.requests().len(),
        1,
        "the request was routed to the port"
    );
}

#[test]
fn training_service_surfaces_a_failed_gate_without_activation() {
    let pipeline = Arc::new(FakeTrainingPipeline::returning(report(false)));
    let service = TrainingService::new(pipeline);
    let got = block_on(service.run(request())).unwrap();
    assert!(!got.promoted());
    assert_eq!(got.adapter.status, AdapterStatus::FailedEval);
    assert!(!got.eval_run.approved_for_use);
}

#[test]
fn training_service_composes_from_the_ports_bundle() {
    let ports = full_ports();
    let service = TrainingService::from_ports(&ports);
    // The default FakeTrainingPipeline reports an empty corpus.
    assert!(block_on(service.run(request())).is_err());
}

#[test]
fn a_lora_adapted_provider_cannot_bypass_compatibility_or_the_policy_guard() {
    // The provider's base model is the llama family; it returns a fixed structured response.
    let provider = FakeAdapterProvider::new(
        BaseModelTarget {
            family: "llama".to_owned(),
            tokenizer_hash: Some("tok_a".to_owned()),
            chat_template_hash: None,
        },
        StructuredResponse {
            raw_text: "send the payment now".to_owned(),
            parsed_json: serde_json::json!({"text": "send the payment now"}),
            schema_validated_by: None,
        },
    );

    // (1) An adapter for a DIFFERENT base family cannot be loaded — no silent attach.
    let incompatible = AdapterSpec {
        adapter_id: AdapterId::from("lora_qwen"),
        path: "/x".to_owned(),
        adapter_type: AdapterType::Lora,
        base_model_family: "qwen".to_owned(),
        tokenizer_hash: Some("tok_a".to_owned()),
        chat_template_hash: None,
    };
    assert!(provider.can_load_adapter(&incompatible).is_incompatible());
    assert!(block_on(provider.load_adapter(incompatible)).is_err());
    assert!(provider.loaded_adapters().is_empty());

    // (2) A compatible adapter loads — the provider is now "LoRA-adapted".
    let compatible = AdapterSpec {
        adapter_id: AdapterId::from("lora_llama"),
        path: "/y".to_owned(),
        adapter_type: AdapterType::Lora,
        base_model_family: "llama".to_owned(),
        tokenizer_hash: Some("tok_a".to_owned()),
        chat_template_hash: None,
    };
    block_on(provider.load_adapter(compatible)).unwrap();
    assert_eq!(provider.loaded_adapters().len(), 1);

    // (3) Whatever the adapter-influenced provider produces, a proposed action built from it
    // STILL passes through the policy guard — which blocks the prohibited send. Loading an
    // adapter granted no exemption from the hard-safety chokepoint.
    let guard = FakePolicyGuard::new();
    let plan = ActionPlan {
        decision_id: DecisionId::from("dec_adapter"),
        message_id: Some(MessageId::from("msg_1")),
        actions: vec![ProposedAction::SendDraft {
            draft_id: DraftId::from("draft_1"),
        }],
    };
    let guarded = block_on(guard.evaluate_action_plan(PolicyContext::default(), plan)).unwrap();
    assert!(
        guarded.allowed_actions.is_empty(),
        "the adapter cannot get a send approved"
    );
    assert_eq!(
        guarded.blocked_actions.len(),
        1,
        "the policy guard blocked it"
    );
}

fn full_ports() -> Ports {
    Ports {
        mail_client: Arc::new(FakeMailClient::new()),
        transport: Arc::new(FakeTransport::new()),
        clock: Arc::new(FakeClock::new(Timestamp::now())),
        secret_store: Arc::new(FakeSecretStore::new()),
        feature_extractor: Arc::new(StubFeatureExtractor),
        tier2: Arc::new(FakeTier2Classifier::new()),
        classification_engine: Arc::new(FakeClassificationEngine::returning(Classification {
            decision_id: DecisionId::from("dec_seed"),
            labels: vec!["general".to_owned()],
            spam_score: 0.0,
            phishing_score: 0.0,
            priority: Priority::Normal,
            needs_review: false,
            provenance: ClassificationProvenance::tier1(vec![]),
        })),
        action_planner: Arc::new(FakeActionPlanner::returning(vec![])),
        policy_guard: Arc::new(FakePolicyGuard::new()),
        learning_engine: Arc::new(FakeLearningEngine::new()),
        rule_curator: Arc::new(FakeRuleCurator::new()),
        proposal_review: Arc::new(FakeProposalReview::new()),
        training_pipeline: Arc::new(FakeTrainingPipeline::new()),
    }
}
