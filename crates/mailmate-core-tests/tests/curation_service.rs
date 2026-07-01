//! Behavioural tests for the curation use-cases: [`CurationService`] drives the curator and
//! returns its report; [`ReviewService`] lists the pending queue and routes a decision to the
//! review port. Driven over the in-memory fakes (no provider, no storage backend).

use std::sync::Arc;

use futures::executor::block_on;

use mailmate_common::curator::{CuratorOperation, CuratorReport, CuratorRequest, ReviewDecision};
use mailmate_common::evidence::EvidenceSourceKind;
use mailmate_common::ids::{FeedbackId, ProposalId};
use mailmate_common::proposal::{AgentProposal, EvidenceRef, ProposalKind, ProposalStatus};
use mailmate_common::rules::rule::{RiskLevel, RuleStatus};
use mailmate_common::time::Timestamp;
use mailmate_core::{CurationService, Ports, ReviewService};
use mailmate_test_support::fakes::{
    FakeActionPlanner, FakeClassificationEngine, FakeClock, FakeLearningEngine, FakeMailClient,
    FakePolicyGuard, FakeProposalReview, FakeReplyDrafter, FakeRuleCurator, FakeSecretStore,
    FakeTier2Classifier, FakeTrainingPipeline, FakeTransport, StubFeatureExtractor,
};

fn a_proposal(id: &str) -> AgentProposal {
    AgentProposal {
        id: ProposalId::from(id),
        proposal_type: ProposalKind::NewRule,
        status: ProposalStatus::PendingReview,
        title: "File stripe.com to Receipts".to_owned(),
        rationale: "6 moves to Receipts".to_owned(),
        risk_level: RiskLevel::Low,
        recommended_status: RuleStatus::ShadowMode,
        rule_draft: None,
        target_rule_kind: None,
        target_rule_id: None,
        workflow_draft: None,
        target_workflow_id: None,
        evidence_refs: vec![EvidenceRef {
            kind: EvidenceSourceKind::Filing,
            id: FeedbackId::from("filfb_1"),
        }],
        back_test: None,
        conflicts: Vec::new(),
        source_provider: "ai-curator".to_owned(),
        created_at: Timestamp::now(),
        reviewed_at: None,
    }
}

#[test]
fn curation_service_runs_the_curator_and_returns_its_report() {
    let report = CuratorReport {
        proposals: vec![a_proposal("prop_1")],
        ..CuratorReport::default()
    };
    let curator = Arc::new(FakeRuleCurator::returning(report));
    let service = CurationService::new(curator.clone());

    let got = block_on(service.run(CuratorRequest::just(CuratorOperation::Propose))).unwrap();
    assert_eq!(got.proposals.len(), 1);
    assert_eq!(got.proposals[0].id, ProposalId::from("prop_1"));
    // The request was routed to the curator port.
    assert_eq!(curator.requests().len(), 1);
    assert_eq!(
        curator.requests()[0].operations,
        vec![CuratorOperation::Propose]
    );
}

#[test]
fn review_service_lists_the_queue_and_routes_a_decision() {
    let review = Arc::new(FakeProposalReview::new().with_pending(vec![a_proposal("prop_q")]));
    let service = ReviewService::new(review.clone());

    // The queue surfaces the pending proposal.
    let queue = block_on(service.queue()).unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].id, ProposalId::from("prop_q"));

    // A decision is routed to the review port and its outcome is returned.
    let outcome =
        block_on(service.decide(ReviewDecision::accept(ProposalId::from("prop_q")))).unwrap();
    assert_eq!(outcome.new_status, ProposalStatus::Accepted);
    assert_eq!(outcome.proposal_id, ProposalId::from("prop_q"));
    assert_eq!(review.decisions().len(), 1);

    // A rejection is likewise routed.
    let outcome = block_on(service.decide(ReviewDecision::reject(
        ProposalId::from("prop_q"),
        "too_broad",
    )))
    .unwrap();
    assert_eq!(outcome.new_status, ProposalStatus::Rejected);
    assert_eq!(review.decisions().len(), 2);
    // The fake never materializes a rule; that is the real adapter's job (tested there).
    assert!(outcome.created_rule_id.is_none());
}

#[test]
fn services_assemble_from_the_ports_bundle() {
    use mailmate_common::classification::{Classification, ClassificationProvenance, Priority};
    use mailmate_common::ids::DecisionId;

    let ports = Ports {
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
            confidence: 0.0,
            salient_signals: Vec::new(),
            safety_findings: Vec::new(),
            provenance: ClassificationProvenance::tier1(vec![]),
        })),
        action_planner: Arc::new(FakeActionPlanner::returning(vec![])),
        policy_guard: Arc::new(FakePolicyGuard::new()),
        learning_engine: Arc::new(FakeLearningEngine::new()),
        rule_curator: Arc::new(FakeRuleCurator::new()),
        proposal_review: Arc::new(FakeProposalReview::new()),
        reply_drafter: Arc::new(FakeReplyDrafter::new()),
        training_pipeline: Arc::new(FakeTrainingPipeline::new()),
    };

    // Both services compose from the bundle and route through their ports.
    let curation = CurationService::from_ports(&ports);
    assert!(block_on(curation.run(CuratorRequest::full()))
        .unwrap()
        .is_empty());
    let review = ReviewService::from_ports(&ports);
    assert!(block_on(review.queue()).unwrap().is_empty());
}
