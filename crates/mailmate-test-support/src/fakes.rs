//! Hand-written, deterministic, call-recording fakes implementing the Phase-0 ports.
//!
//! Each fake uses interior mutability (the port methods take `&self`), records what it
//! was asked to do for assertions, and — crucially — never performs real I/O. No fake
//! ever locks across an `await`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;

use mailmate_common::action::{ActionPlan, BlockedAction, GuardedActionPlan, ProposedAction};
use mailmate_common::classification::{Classification, ClassificationInput};
use mailmate_common::error::{
    ActionPlanningError, ClassificationError, MailError, MlError, PolicyError, SecretError,
};
use mailmate_common::features::{CalibratedScores, FeatureValue, FeatureVector, LabeledExample};
use mailmate_common::ids::{DraftId, MessageId};
use mailmate_common::mail::{DraftSpec, FetchScope, MailAction, MailEvent, MessageData};
use mailmate_common::planning::ActionPlanningInput;
use mailmate_common::policy::{PolicyCheckResult, PolicyContext, PolicyOutcome};
use mailmate_common::protocol::Frame;
use mailmate_common::secret::{Secret, SecretKey};
use mailmate_common::stream::{EventStream, FrameStream};
use mailmate_common::time::Timestamp;
use mailmate_ports::action_planner::ActionPlanner;
use mailmate_ports::classification_engine::ClassificationEngine;
use mailmate_ports::clock::Clock;
use mailmate_ports::feature_extractor::FeatureExtractor;
use mailmate_ports::mail_client::MailClient;
use mailmate_ports::policy_guard::PolicyGuard;
use mailmate_ports::secret_store::SecretStore;
use mailmate_ports::tier2_classifier::Tier2Classifier;
use mailmate_ports::transport::Transport;

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
}
