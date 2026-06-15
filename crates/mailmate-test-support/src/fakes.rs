//! Hand-written, deterministic, call-recording fakes implementing the Phase-0 ports.
//!
//! Each fake uses interior mutability (the port methods take `&self`), records what it
//! was asked to do for assertions, and — crucially — never performs real I/O. No fake
//! ever locks across an `await`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;

use mailmate_common::action::{ActionPlan, BlockedAction, GuardedActionPlan, ProposedAction};
use mailmate_common::adapter::{
    check_compatibility, AdapterCompatibility, AdapterSpec, BaseModelTarget,
};
use mailmate_common::ai::{
    ProviderCapabilities, ProviderId, StructuredRequest, StructuredResponse,
};
use mailmate_common::audit::AuditEntry;
use mailmate_common::classification::{Classification, ClassificationInput};
use mailmate_common::curator::{CuratorReport, CuratorRequest, ReviewDecision, ReviewOutcome};
use mailmate_common::error::{
    ActionPlanningError, AiError, ClassificationError, CuratorError, LearningError, MailError,
    MlError, PolicyError, ReviewError, SecretError, TrainingError,
};
use mailmate_common::evidence::{EvidenceQuery, RuleEvidence};
use mailmate_common::features::{CalibratedScores, FeatureValue, FeatureVector, LabeledExample};
use mailmate_common::feedback::TaskFeedback;
use mailmate_common::ids::{AdapterId, AuditId, DraftId, FeedbackId, MessageId};
use mailmate_common::mail::{DraftSpec, FetchScope, MailAction, MailEvent, MessageData};
use mailmate_common::planning::ActionPlanningInput;
use mailmate_common::policy::{PolicyCheckResult, PolicyContext, PolicyOutcome};
use mailmate_common::proposal::{AgentProposal, ProposalStatus, ProposalTrigger};
use mailmate_common::protocol::Frame;
use mailmate_common::secret::{Secret, SecretKey};
use mailmate_common::stream::{EventStream, FrameStream};
use mailmate_common::time::Timestamp;
use mailmate_ports::action_planner::ActionPlanner;
use mailmate_ports::ai_provider::{AiProvider, SupportsAdapters};
use mailmate_ports::classification_engine::ClassificationEngine;
use mailmate_ports::clock::Clock;
use mailmate_ports::feature_extractor::FeatureExtractor;
use mailmate_ports::learning_engine::LearningEngine;
use mailmate_ports::mail_client::MailClient;
use mailmate_ports::policy_guard::PolicyGuard;
use mailmate_ports::proposal_review::ProposalReview;
use mailmate_ports::rule_curator::RuleCurator;
use mailmate_ports::secret_store::SecretStore;
use mailmate_ports::tier2_classifier::Tier2Classifier;
use mailmate_ports::training_pipeline::TrainingPipeline;
use mailmate_ports::transport::Transport;

use mailmate_common::training::{TrainingPipelineReport, TrainingPipelineRequest};

// ---------------------------------------------------------------------------
// FakeMailClient
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct MailClientState {
    applied: Vec<MailAction>,
    drafts: Vec<DraftSpec>,
    messages: HashMap<String, MessageData>,
    events: Vec<MailEvent>,
    draft_counter: u64,
}

/// An in-memory `MailClient` that records actions/drafts and never sends.
#[derive(Debug, Default)]
pub struct FakeMailClient {
    state: Mutex<MailClientState>,
}

impl FakeMailClient {
    /// A fresh, empty fake client.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a message so `fetch` can return it. The message's `id` must be set.
    pub fn seed_message(&self, message: MessageData) {
        let id = message.id.clone().expect("seeded message must have an id");
        self.state
            .lock()
            .unwrap()
            .messages
            .insert(id.into_string(), message);
    }

    /// Script the events `events()` will yield (in order).
    pub fn script_events(&self, events: Vec<MailEvent>) {
        self.state.lock().unwrap().events = events;
    }

    /// The actions applied so far, in order.
    #[must_use]
    pub fn applied_actions(&self) -> Vec<MailAction> {
        self.state.lock().unwrap().applied.clone()
    }

    /// The drafts created so far, in order.
    #[must_use]
    pub fn created_drafts(&self) -> Vec<DraftSpec> {
        self.state.lock().unwrap().drafts.clone()
    }
}

#[async_trait]
impl MailClient for FakeMailClient {
    async fn apply(&self, action: MailAction) -> Result<(), MailError> {
        self.state.lock().unwrap().applied.push(action);
        Ok(())
    }

    async fn create_draft(&self, spec: DraftSpec) -> Result<DraftId, MailError> {
        let mut state = self.state.lock().unwrap();
        state.draft_counter += 1;
        let id = DraftId::from(format!("draft_{}", state.draft_counter));
        state.drafts.push(spec);
        Ok(id)
    }

    async fn fetch(&self, id: MessageId, _scope: FetchScope) -> Result<MessageData, MailError> {
        self.state
            .lock()
            .unwrap()
            .messages
            .get(id.as_str())
            .cloned()
            .ok_or(MailError::NotFound(id))
    }

    fn events(&self) -> EventStream<MailEvent> {
        let events = self.state.lock().unwrap().events.clone();
        Box::pin(futures::stream::iter(events))
    }
}

// ---------------------------------------------------------------------------
// FakeTransport
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct TransportState {
    sent: Vec<Frame>,
    inbound: Vec<Frame>,
}

/// An in-process `Transport`: records sent frames, yields scripted inbound frames once.
#[derive(Debug, Default)]
pub struct FakeTransport {
    state: Mutex<TransportState>,
}

impl FakeTransport {
    /// A fresh, empty fake transport.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Script the frames `incoming()` will yield (consumed on first call).
    pub fn script_inbound(&self, frames: Vec<Frame>) {
        self.state.lock().unwrap().inbound = frames;
    }

    /// The frames sent so far, in order.
    #[must_use]
    pub fn sent_frames(&self) -> Vec<Frame> {
        self.state.lock().unwrap().sent.clone()
    }
}

impl Transport for FakeTransport {
    fn send(&self, frame: Frame) -> Result<(), mailmate_common::error::TransportError> {
        self.state.lock().unwrap().sent.push(frame);
        Ok(())
    }

    fn incoming(&self) -> FrameStream {
        let frames = std::mem::take(&mut self.state.lock().unwrap().inbound);
        Box::pin(futures::stream::iter(frames.into_iter().map(Ok)))
    }
}

// ---------------------------------------------------------------------------
// FakeClock
// ---------------------------------------------------------------------------

/// A settable, advanceable clock for deterministic time-dependent tests.
#[derive(Debug)]
pub struct FakeClock {
    now: Mutex<Timestamp>,
}

impl FakeClock {
    /// A clock starting at `start`.
    #[must_use]
    pub fn new(start: Timestamp) -> Self {
        Self {
            now: Mutex::new(start),
        }
    }

    /// Set the current time.
    pub fn set(&self, instant: Timestamp) {
        *self.now.lock().unwrap() = instant;
    }

    /// Move the clock forward by `delta`.
    pub fn advance(&self, delta: time::Duration) {
        let mut guard = self.now.lock().unwrap();
        *guard = Timestamp(guard.0 + delta);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        *self.now.lock().unwrap()
    }
}

// ---------------------------------------------------------------------------
// FakeSecretStore
// ---------------------------------------------------------------------------

/// An in-memory `SecretStore`.
#[derive(Debug, Default)]
pub struct FakeSecretStore {
    entries: Mutex<HashMap<SecretKey, Secret>>,
}

impl FakeSecretStore {
    /// A fresh, empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SecretStore for FakeSecretStore {
    async fn get(&self, key: SecretKey) -> Result<Option<Secret>, SecretError> {
        Ok(self.entries.lock().unwrap().get(&key).cloned())
    }

    async fn put(&self, key: SecretKey, value: Secret) -> Result<(), SecretError> {
        self.entries.lock().unwrap().insert(key, value);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// StubFeatureExtractor
// ---------------------------------------------------------------------------

/// A pure, deterministic `FeatureExtractor` computing a few non-body features.
#[derive(Debug, Default)]
pub struct StubFeatureExtractor;

impl FeatureExtractor for StubFeatureExtractor {
    fn extract(&self, msg: &MessageData) -> FeatureVector {
        let mut fv = FeatureVector::new();
        fv.insert(
            "subject_len",
            FeatureValue::Number(msg.headers.subject.chars().count() as f64),
        );
        fv.insert(
            "has_attachments",
            FeatureValue::Bool(!msg.attachments.is_empty()),
        );
        fv.insert("from", FeatureValue::Text(msg.headers.from.clone()));
        fv
    }
}

// ---------------------------------------------------------------------------
// FakeTier2Classifier
// ---------------------------------------------------------------------------

/// A deterministic `Tier2Classifier`: fixed scores, records the examples it is fed.
#[derive(Debug, Default)]
pub struct FakeTier2Classifier {
    updates: Mutex<Vec<LabeledExample>>,
}

impl FakeTier2Classifier {
    /// A fresh classifier.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The examples passed to `update`, in order.
    #[must_use]
    pub fn observed_updates(&self) -> Vec<LabeledExample> {
        self.updates.lock().unwrap().clone()
    }
}

#[async_trait]
impl Tier2Classifier for FakeTier2Classifier {
    async fn predict(&self, _features: FeatureVector) -> Result<CalibratedScores, MlError> {
        let mut scores = BTreeMap::new();
        scores.insert("ham".to_owned(), 0.9);
        scores.insert("spam".to_owned(), 0.1);
        Ok(CalibratedScores {
            scores,
            calibration_version: "fake-v1".to_owned(),
        })
    }

    async fn update(&self, labeled: LabeledExample) -> Result<(), MlError> {
        self.updates.lock().unwrap().push(labeled);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// FakeClassificationEngine
// ---------------------------------------------------------------------------

/// A `ClassificationEngine` that returns a fixed verdict and records what it was asked to
/// classify — so a use-case test can prove the classify step was driven without wiring the
/// real cascade.
#[derive(Debug)]
pub struct FakeClassificationEngine {
    verdict: Classification,
    seen: Mutex<Vec<MessageId>>,
}

impl FakeClassificationEngine {
    /// A classifier that always returns `verdict`.
    #[must_use]
    pub fn returning(verdict: Classification) -> Self {
        Self {
            verdict,
            seen: Mutex::new(Vec::new()),
        }
    }

    /// The internal ids of the messages it was asked to classify, in order.
    #[must_use]
    pub fn classified(&self) -> Vec<MessageId> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl ClassificationEngine for FakeClassificationEngine {
    async fn classify(
        &self,
        input: ClassificationInput,
    ) -> Result<Classification, ClassificationError> {
        if let Some(id) = input.message.id.clone() {
            self.seen.lock().unwrap().push(id);
        }
        // Keep the verdict tied to the decision the caller opened.
        let mut verdict = self.verdict.clone();
        verdict.decision_id = input.decision_id;
        Ok(verdict)
    }
}

// ---------------------------------------------------------------------------
// FakeActionPlanner
// ---------------------------------------------------------------------------

/// An `ActionPlanner` that returns a fixed candidate action list and records the inputs.
#[derive(Debug)]
pub struct FakeActionPlanner {
    actions: Vec<ProposedAction>,
    seen: Mutex<Vec<ActionPlanningInput>>,
}

impl FakeActionPlanner {
    /// A planner that always proposes `actions`.
    #[must_use]
    pub fn returning(actions: Vec<ProposedAction>) -> Self {
        Self {
            actions,
            seen: Mutex::new(Vec::new()),
        }
    }

    /// The planning inputs it was given, in order.
    #[must_use]
    pub fn planned(&self) -> Vec<ActionPlanningInput> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl ActionPlanner for FakeActionPlanner {
    async fn plan(&self, input: ActionPlanningInput) -> Result<ActionPlan, ActionPlanningError> {
        let plan = ActionPlan {
            decision_id: input.decision_id.clone(),
            message_id: input.message.id.clone(),
            actions: self.actions.clone(),
        };
        self.seen.lock().unwrap().push(input);
        Ok(plan)
    }
}

// ---------------------------------------------------------------------------
// FakePolicyGuard
// ---------------------------------------------------------------------------

/// A minimal `PolicyGuard`: every candidate that projects onto a safe `PlannedAction` is
/// allowed; a prohibited candidate is blocked. It deliberately omits the sensitive-category
/// review logic (that is the real `HardPolicyGuard`'s job) — enough to prove a use-case
/// routes a plan through the guard.
#[derive(Debug, Default)]
pub struct FakePolicyGuard;

impl FakePolicyGuard {
    /// A fresh guard.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl PolicyGuard for FakePolicyGuard {
    async fn evaluate_action_plan(
        &self,
        _context: PolicyContext,
        plan: ActionPlan,
    ) -> Result<GuardedActionPlan, PolicyError> {
        let mut allowed_actions = Vec::new();
        let mut blocked_actions = Vec::new();
        let mut policy_checks = Vec::new();
        for action in plan.actions {
            match action.to_planned() {
                Some(planned) => {
                    policy_checks.push(PolicyCheckResult {
                        policy_id: "fake_no_restriction".to_owned(),
                        outcome: PolicyOutcome::Allowed,
                    });
                    allowed_actions.push(planned);
                }
                None => {
                    policy_checks.push(PolicyCheckResult {
                        policy_id: "fake_prohibited".to_owned(),
                        outcome: PolicyOutcome::Blocked {
                            policy_id: "fake_prohibited".to_owned(),
                            reason: "prohibited action".to_owned(),
                        },
                    });
                    blocked_actions.push(BlockedAction {
                        action,
                        policy_id: "fake_prohibited".to_owned(),
                        reason: "prohibited action".to_owned(),
                    });
                }
            }
        }
        Ok(GuardedActionPlan {
            decision_id: plan.decision_id,
            allowed_actions,
            review_required_actions: Vec::new(),
            blocked_actions,
            policy_checks,
        })
    }
}

// ---------------------------------------------------------------------------
// FakeLearningEngine
// ---------------------------------------------------------------------------

/// A `LearningEngine` that records every captured correction and audit entry, and returns
/// pre-seeded evidence/proposals — so a use-case test can prove the capture step was driven
/// without wiring the real engine and repositories.
#[derive(Debug, Default)]
pub struct FakeLearningEngine {
    feedback: Mutex<Vec<TaskFeedback>>,
    audits: Mutex<Vec<AuditEntry>>,
    evidence: Vec<RuleEvidence>,
    proposals: Vec<AgentProposal>,
}

impl FakeLearningEngine {
    /// A fresh engine that captures everything and proposes nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-seed the evidence `collect_evidence` returns.
    #[must_use]
    pub fn with_evidence(mut self, evidence: Vec<RuleEvidence>) -> Self {
        self.evidence = evidence;
        self
    }

    /// Pre-seed the proposals `propose_candidates` returns.
    #[must_use]
    pub fn with_proposals(mut self, proposals: Vec<AgentProposal>) -> Self {
        self.proposals = proposals;
        self
    }

    /// The corrections it captured, in order.
    #[must_use]
    pub fn recorded_feedback(&self) -> Vec<TaskFeedback> {
        self.feedback.lock().unwrap().clone()
    }

    /// The audit entries it captured, in order.
    #[must_use]
    pub fn recorded_audits(&self) -> Vec<AuditEntry> {
        self.audits.lock().unwrap().clone()
    }
}

#[async_trait]
impl LearningEngine for FakeLearningEngine {
    async fn record_feedback(&self, feedback: TaskFeedback) -> Result<FeedbackId, LearningError> {
        let id = feedback.id().clone();
        self.feedback.lock().unwrap().push(feedback);
        Ok(id)
    }

    async fn record_audit(&self, entry: AuditEntry) -> Result<AuditId, LearningError> {
        let id = entry.id.clone();
        self.audits.lock().unwrap().push(entry);
        Ok(id)
    }

    async fn collect_evidence(
        &self,
        _query: EvidenceQuery,
    ) -> Result<Vec<RuleEvidence>, LearningError> {
        Ok(self.evidence.clone())
    }

    async fn propose_candidates(
        &self,
        _trigger: ProposalTrigger,
    ) -> Result<Vec<AgentProposal>, LearningError> {
        Ok(self.proposals.clone())
    }
}

// ---------------------------------------------------------------------------
// FakeRuleCurator
// ---------------------------------------------------------------------------

/// A `RuleCurator` that records the requests it was given and returns a pre-seeded report —
/// so a use-case test can prove the curation step was driven without wiring the real curator,
/// provider, and repositories.
#[derive(Debug, Default)]
pub struct FakeRuleCurator {
    report: CuratorReport,
    seen: Mutex<Vec<CuratorRequest>>,
}

impl FakeRuleCurator {
    /// A curator that records requests and returns an empty report.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A curator that returns `report` for every pass.
    #[must_use]
    pub fn returning(report: CuratorReport) -> Self {
        Self {
            report,
            seen: Mutex::new(Vec::new()),
        }
    }

    /// The requests it was given, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<CuratorRequest> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl RuleCurator for FakeRuleCurator {
    async fn curate(&self, request: CuratorRequest) -> Result<CuratorReport, CuratorError> {
        self.seen.lock().unwrap().push(request);
        Ok(self.report.clone())
    }
}

// ---------------------------------------------------------------------------
// FakeProposalReview
// ---------------------------------------------------------------------------

/// A `ProposalReview` that records the decisions it was given and returns a synthesized
/// outcome (accepting → `Accepted`, otherwise → `Rejected`; never creating a rule). It can be
/// pre-seeded with a pending queue.
#[derive(Debug, Default)]
pub struct FakeProposalReview {
    pending: Vec<AgentProposal>,
    decisions: Mutex<Vec<ReviewDecision>>,
}

impl FakeProposalReview {
    /// A review adapter with an empty pending queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-seed the proposals `pending` returns.
    #[must_use]
    pub fn with_pending(mut self, pending: Vec<AgentProposal>) -> Self {
        self.pending = pending;
        self
    }

    /// The decisions it was given, in order.
    #[must_use]
    pub fn decisions(&self) -> Vec<ReviewDecision> {
        self.decisions.lock().unwrap().clone()
    }
}

#[async_trait]
impl ProposalReview for FakeProposalReview {
    async fn pending(&self) -> Result<Vec<AgentProposal>, ReviewError> {
        Ok(self.pending.clone())
    }

    async fn review(&self, decision: ReviewDecision) -> Result<ReviewOutcome, ReviewError> {
        let proposal_id = decision.proposal_id.clone();
        let new_status = if decision.outcome.is_acceptance() {
            ProposalStatus::Accepted
        } else {
            ProposalStatus::Rejected
        };
        self.decisions.lock().unwrap().push(decision);
        Ok(ReviewOutcome {
            proposal_id,
            new_status,
            created_rule_id: None,
            feedback_id: FeedbackId::from("rpffb_fake"),
        })
    }
}

// ---------------------------------------------------------------------------
// FakeTrainingPipeline
// ---------------------------------------------------------------------------

/// A `TrainingPipeline` that records the requests it was given and returns a pre-seeded
/// report — so a use-case test can prove the training step was driven without wiring the real
/// pipeline, trainer backend, and repositories. With no seeded report it returns a benign
/// export error (an empty feedback corpus produces nothing to train on).
#[derive(Debug, Default)]
pub struct FakeTrainingPipeline {
    report: Option<TrainingPipelineReport>,
    seen: Mutex<Vec<TrainingPipelineRequest>>,
}

impl FakeTrainingPipeline {
    /// A pipeline that records requests and returns an "empty corpus" error.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A pipeline that returns `report` for every run.
    #[must_use]
    pub fn returning(report: TrainingPipelineReport) -> Self {
        Self {
            report: Some(report),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// The requests it was given, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<TrainingPipelineRequest> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl TrainingPipeline for FakeTrainingPipeline {
    async fn run(
        &self,
        request: TrainingPipelineRequest,
    ) -> Result<TrainingPipelineReport, TrainingError> {
        self.seen.lock().unwrap().push(request);
        match &self.report {
            Some(report) => Ok(report.clone()),
            None => Err(TrainingError::Export(
                "fake: no eligible examples".to_owned(),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// FakeAdapterProvider
// ---------------------------------------------------------------------------

/// An `AiProvider` that also `SupportsAdapters`, for proving a LoRA-adapted provider is still
/// just a provider: it refuses an incompatible adapter (no silent activation), and whatever
/// it returns after an adapter is loaded is the *same* `StructuredResponse` every other
/// provider returns — so it still flows through validation → rules → policy and cannot bypass
/// them. The response it returns is fixed at construction (so a test can make it emit a
/// policy-forbidden suggestion and prove the guard still blocks it).
#[derive(Debug)]
pub struct FakeAdapterProvider {
    target: BaseModelTarget,
    response: StructuredResponse,
    loaded: Mutex<Vec<AdapterId>>,
}

impl FakeAdapterProvider {
    /// A provider whose base model is `target` and which always returns `response`.
    #[must_use]
    pub fn new(target: BaseModelTarget, response: StructuredResponse) -> Self {
        Self {
            target,
            response,
            loaded: Mutex::new(Vec::new()),
        }
    }

    /// The adapters currently loaded.
    #[must_use]
    pub fn loaded_adapters(&self) -> Vec<AdapterId> {
        self.loaded.lock().unwrap().clone()
    }
}

#[async_trait]
impl AiProvider for FakeAdapterProvider {
    fn id(&self) -> ProviderId {
        ProviderId::from("prov_adapter_fake")
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    async fn complete_structured(
        &self,
        _request: StructuredRequest,
    ) -> Result<StructuredResponse, AiError> {
        Ok(self.response.clone())
    }
}

#[async_trait]
impl SupportsAdapters for FakeAdapterProvider {
    fn can_load_adapter(&self, adapter: &AdapterSpec) -> AdapterCompatibility {
        check_compatibility(adapter, &self.target)
    }

    async fn load_adapter(&self, adapter: AdapterSpec) -> Result<(), AiError> {
        // An incompatible adapter is REFUSED — it cannot attach to a base it does not match.
        if check_compatibility(&adapter, &self.target).is_incompatible() {
            return Err(AiError::Validation(format!(
                "refusing to load incompatible adapter {}",
                adapter.adapter_id
            )));
        }
        self.loaded.lock().unwrap().push(adapter.adapter_id);
        Ok(())
    }

    async fn unload_adapter(&self, adapter_id: AdapterId) -> Result<(), AiError> {
        self.loaded.lock().unwrap().retain(|id| id != &adapter_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::sample_message;
    use futures::executor::block_on;
    use mailmate_common::protocol::{Frame, ProtocolVersion};

    #[test]
    fn fake_mail_client_records_actions_and_drafts_and_never_sends() {
        let client = FakeMailClient::new();
        block_on(client.apply(MailAction::MarkRead {
            message_id: MessageId::from("msg_1"),
            read: true,
        }))
        .unwrap();
        let id = block_on(client.create_draft(DraftSpec::default())).unwrap();
        assert_eq!(id.as_str(), "draft_1", "draft ids are deterministic");
        assert_eq!(client.applied_actions().len(), 1);
        assert_eq!(client.created_drafts().len(), 1);
    }

    #[test]
    fn fake_mail_client_fetch_hits_seeded_and_misses_otherwise() {
        let client = FakeMailClient::new();
        client.seed_message(sample_message());
        let got = block_on(client.fetch(MessageId::from("msg_sample"), FetchScope::Full)).unwrap();
        assert_eq!(got.headers.subject, "Quote request");
        let missing = block_on(client.fetch(MessageId::from("msg_nope"), FetchScope::Full));
        assert!(matches!(missing, Err(MailError::NotFound(_))));
    }

    #[test]
    fn fake_mail_client_streams_scripted_events_in_order() {
        use futures::StreamExt;
        let client = FakeMailClient::new();
        client.script_events(vec![MailEvent::NewMail {
            message: Box::new(sample_message()),
        }]);
        let collected: Vec<MailEvent> = block_on(client.events().collect());
        assert_eq!(collected.len(), 1);
    }

    #[test]
    fn fake_transport_records_sent_and_yields_scripted_inbound_once() {
        use futures::StreamExt;
        let transport = FakeTransport::new();
        let frame = Frame::Notification {
            protocol_version: ProtocolVersion::default(),
            notification_id: "n1".to_owned(),
            type_: "classification_ready".to_owned(),
            payload: serde_json::json!({}),
        };
        transport.send(frame.clone()).unwrap();
        assert_eq!(transport.sent_frames(), vec![frame.clone()]);

        transport.script_inbound(vec![frame]);
        let inbound: Vec<_> = block_on(transport.incoming().collect());
        assert_eq!(inbound.len(), 1);
        assert!(inbound[0].is_ok());
        // Consumed on first call.
        let again: Vec<_> = block_on(transport.incoming().collect());
        assert!(again.is_empty());
    }

    #[test]
    fn fake_clock_is_settable_and_advanceable() {
        let start = Timestamp::now();
        let clock = FakeClock::new(start);
        assert_eq!(clock.now(), start);
        clock.advance(time::Duration::hours(3));
        assert!(clock.now() > start);
        clock.set(start);
        assert_eq!(clock.now(), start);
    }

    #[test]
    fn fake_secret_store_round_trips_and_misses() {
        let store = FakeSecretStore::new();
        let key = SecretKey::from("ollama_api_key");
        assert!(block_on(store.get(key.clone())).unwrap().is_none());
        block_on(store.put(key.clone(), Secret::new("tok"))).unwrap();
        assert_eq!(block_on(store.get(key)).unwrap().unwrap().expose(), "tok");
    }

    #[test]
    fn stub_feature_extractor_is_pure() {
        let extractor = StubFeatureExtractor;
        let msg = sample_message();
        let a = extractor.extract(&msg);
        let b = extractor.extract(&msg);
        assert_eq!(a, b, "extraction must be deterministic");
        assert_eq!(a.get("has_attachments"), Some(&FeatureValue::Bool(true)));
    }

    #[test]
    fn fake_tier2_predicts_deterministically_and_records_updates() {
        let clf = FakeTier2Classifier::new();
        let scores = block_on(clf.predict(FeatureVector::new())).unwrap();
        assert_eq!(scores.scores.get("ham"), Some(&0.9));
        block_on(clf.update(LabeledExample {
            features: FeatureVector::new(),
            label: "spam".to_owned(),
        }))
        .unwrap();
        assert_eq!(clf.observed_updates().len(), 1);
    }

    fn a_classification() -> Classification {
        use mailmate_common::classification::{ClassificationProvenance, Priority};
        use mailmate_common::ids::DecisionId;
        Classification {
            decision_id: DecisionId::from("dec_seed"),
            labels: vec!["general".to_owned()],
            spam_score: 0.0,
            phishing_score: 0.0,
            priority: Priority::Normal,
            needs_review: false,
            provenance: ClassificationProvenance::tier1(vec![]),
        }
    }

    #[test]
    fn fake_classification_engine_returns_its_verdict_and_records_the_message() {
        use mailmate_common::ids::DecisionId;
        let engine = FakeClassificationEngine::returning(a_classification());
        let msg = sample_message();
        let input = ClassificationInput {
            decision_id: DecisionId::from("dec_live"),
            message: msg,
            features: FeatureVector::new(),
        };
        let verdict = block_on(engine.classify(input)).unwrap();
        assert_eq!(verdict.labels, vec!["general".to_owned()]);
        // The verdict is retagged to the caller's decision id.
        assert_eq!(verdict.decision_id, DecisionId::from("dec_live"));
        assert_eq!(engine.classified(), vec![MessageId::from("msg_sample")]);
    }

    #[test]
    fn fake_action_planner_echoes_its_actions_and_records_input() {
        use mailmate_common::planning::ActionPlanningInput;
        let planner = FakeActionPlanner::returning(vec![ProposedAction::Tag {
            message_id: MessageId::from("msg_sample"),
            tag: "x".to_owned(),
        }]);
        let input = ActionPlanningInput::new_mail(
            sample_message(),
            a_classification(),
            FeatureVector::new(),
        );
        let plan = block_on(planner.plan(input)).unwrap();
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(planner.planned().len(), 1);
    }

    #[test]
    fn fake_policy_guard_allows_safe_and_blocks_prohibited() {
        use mailmate_common::ids::DecisionId;
        let guard = FakePolicyGuard::new();
        let plan = ActionPlan {
            decision_id: DecisionId::from("dec_1"),
            message_id: Some(MessageId::from("msg_1")),
            actions: vec![
                ProposedAction::Tag {
                    message_id: MessageId::from("msg_1"),
                    tag: "ok".to_owned(),
                },
                ProposedAction::Delete {
                    message_id: MessageId::from("msg_1"),
                },
            ],
        };
        let guarded = block_on(guard.evaluate_action_plan(PolicyContext::default(), plan)).unwrap();
        assert_eq!(guarded.allowed_actions.len(), 1);
        assert_eq!(guarded.blocked_actions.len(), 1);
    }

    #[test]
    fn fake_learning_engine_captures_feedback_and_audit_and_returns_seeds() {
        use mailmate_common::actor::Actor;
        use mailmate_common::audit::AuditEntry;
        use mailmate_common::feedback::FeedbackPolarity;
        use mailmate_common::feedback::{FilingFeedback, FilingFeedbackRow, PinnedVersions};
        use mailmate_common::ids::FolderId;

        let engine = FakeLearningEngine::new();
        let row = FilingFeedbackRow {
            id: FilingFeedback::fresh_id(),
            message_id: MessageId::from("msg_1"),
            pinned_versions: PinnedVersions::default(),
            sender_domain: Some("s.com".to_owned()),
            ai_suggested_folder: None,
            human_chosen_folder: FolderId::from("folder_x"),
            basis: None,
            matched_rule_id: None,
            polarity: FeedbackPolarity::Negative,
            created_at: Timestamp::now(),
        };
        let id = block_on(engine.record_feedback(TaskFeedback::Filing(row))).unwrap();
        assert!(id.as_str().starts_with("filfb_"));
        assert_eq!(engine.recorded_feedback().len(), 1);

        let audit_id = block_on(engine.record_audit(AuditEntry::new("x", Actor::System))).unwrap();
        assert!(audit_id.as_str().starts_with("audit_"));
        assert_eq!(engine.recorded_audits().len(), 1);

        // collect_evidence / propose_candidates echo the seeds (empty by default).
        assert!(block_on(engine.collect_evidence(EvidenceQuery::all()))
            .unwrap()
            .is_empty());
        assert!(block_on(engine.propose_candidates(ProposalTrigger::all()))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn fake_rule_curator_records_requests_and_returns_its_report() {
        use mailmate_common::curator::{CuratorOperation, CuratorReport, CuratorRequest};
        let curator = FakeRuleCurator::returning(CuratorReport::default());
        let report =
            block_on(curator.curate(CuratorRequest::just(CuratorOperation::Propose))).unwrap();
        assert!(report.is_empty());
        assert_eq!(curator.requests().len(), 1);
    }

    #[test]
    fn fake_proposal_review_records_decisions_and_synthesizes_outcomes() {
        use mailmate_common::curator::ReviewDecision;
        use mailmate_common::ids::ProposalId;
        let review = FakeProposalReview::new();
        let outcome =
            block_on(review.review(ReviewDecision::accept(ProposalId::from("prop_1")))).unwrap();
        assert_eq!(outcome.new_status, ProposalStatus::Accepted);
        assert!(outcome.created_rule_id.is_none());
        let outcome =
            block_on(review.review(ReviewDecision::reject(ProposalId::from("prop_2"), "no")))
                .unwrap();
        assert_eq!(outcome.new_status, ProposalStatus::Rejected);
        assert_eq!(review.decisions().len(), 2);
        assert!(block_on(review.pending()).unwrap().is_empty());
    }

    #[test]
    fn fake_training_pipeline_records_requests_and_reports_empty_by_default() {
        let pipeline = FakeTrainingPipeline::new();
        let err = block_on(pipeline.run(TrainingPipelineRequest::new("d", "p"))).unwrap_err();
        assert!(matches!(err, TrainingError::Export(_)));
        assert_eq!(pipeline.requests().len(), 1);
    }

    #[test]
    fn fake_adapter_provider_refuses_incompatible_adapters_but_still_returns_a_response() {
        use mailmate_common::training::AdapterType;
        let target = BaseModelTarget {
            family: "llama".to_owned(),
            tokenizer_hash: Some("tok_a".to_owned()),
            chat_template_hash: None,
        };
        let response = StructuredResponse {
            raw_text: "ok".to_owned(),
            parsed_json: serde_json::json!({"text": "ok"}),
            schema_validated_by: None,
        };
        let provider = FakeAdapterProvider::new(target, response);

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
        assert!(
            provider.loaded_adapters().is_empty(),
            "incompatible adapter never loads"
        );

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
        // Even with an adapter loaded, the provider returns a plain StructuredResponse.
        let out = block_on(provider.complete_structured(StructuredRequest {
            messages: vec![],
            json_schema: None,
            grammar: None,
            sampling: Default::default(),
        }))
        .unwrap();
        assert_eq!(out.raw_text, "ok");
        block_on(provider.unload_adapter(AdapterId::from("lora_llama"))).unwrap();
        assert!(provider.loaded_adapters().is_empty());
    }
}
