//! MailMate core: domain entities and use-cases.
//!
//! The product lives here. It depends ONLY on `mailmate-ports` (and, once use-cases
//! land, `mailmate-common`) — never on an adapter or a backend crate. That constraint
//! is enforced from the outside by `mailmate-arch-test` and the no-backend-leakage CI
//! guard, and core's behavioural tests live in `mailmate-core-tests` so `mailmate-core`
//! keeps zero dev-dependency path to any backend.

use std::sync::Arc;

use mailmate_ports::action_planner::ActionPlanner;
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

pub mod usecases;

pub use usecases::{
    CorrectionContext, CorrectionService, CurationService, PlanningOutcome, PlanningService,
    ReviewService, TrainingService,
};

/// The set of adapters the core's use-cases run against — the dependency-injection
/// seam.
///
/// The core only ever holds *ports* here (`Arc<dyn Port>`); concrete adapters are
/// injected at the edge in production and as fakes in tests, and the core never names
/// either. Cloning is cheap (reference-counted) so use-cases can hold their own handle.
///
/// Construct it with a struct literal (every field is public) — a positional constructor
/// over this many ports would be both error-prone and a `too_many_arguments` lint; named
/// fields read better at the wiring edge.
#[derive(Clone)]
pub struct Ports {
    /// The mail client (Thunderbird in production; a fake in tests).
    pub mail_client: Arc<dyn MailClient>,
    /// The IPC transport.
    pub transport: Arc<dyn Transport>,
    /// The clock (system wall-clock in production; a fake clock in tests).
    pub clock: Arc<dyn Clock>,
    /// The secret store.
    pub secret_store: Arc<dyn SecretStore>,
    /// The deterministic feature extractor.
    pub feature_extractor: Arc<dyn FeatureExtractor>,
    /// The Tier-2 classifier.
    pub tier2: Arc<dyn Tier2Classifier>,
    /// Pipeline 1: the classification engine (the cascade in production).
    pub classification_engine: Arc<dyn ClassificationEngine>,
    /// Pipeline 2: the action planner.
    pub action_planner: Arc<dyn ActionPlanner>,
    /// The policy guard that gates every candidate plan.
    pub policy_guard: Arc<dyn PolicyGuard>,
    /// The learning engine: captures corrections and proposes crystallized rules.
    pub learning_engine: Arc<dyn LearningEngine>,
    /// The AI rule curator: proposes (never activates) rule changes and detects conflicts.
    pub rule_curator: Arc<dyn RuleCurator>,
    /// The proposal-review step: applies a human's accept/reject decision to a proposal.
    pub proposal_review: Arc<dyn ProposalReview>,
    /// The on-device training pipeline: derives datasets, trains and evaluates a candidate
    /// adapter, and gates its promotion (an adapter is advisory, never activated here).
    pub training_pipeline: Arc<dyn TrainingPipeline>,
}

/// Returns this crate's package name for smoke tests.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_available() {
        assert_eq!(super::crate_name(), "mailmate-core");
    }
}
