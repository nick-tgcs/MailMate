//! The curation use-cases: improving the rule system, and disposing of what the curator
//! proposes.
//!
//! Two thin orchestration seams over the curator and review ports. [`CurationService`] runs a
//! curator pass (propose/refine/merge/split/stale + conflict detection + threshold/feedback
//! observations) — the curator only ever *proposes*. [`ReviewService`] is the human gate:
//! listing what awaits a decision and applying an accept/reject, which is the **only** way a
//! proposed rule is ever materialized — and only into shadow/pending, never active.
//!
//! Both name only ports, so the concrete curator (`mailmate-learning::AiRuleCurator`) and
//! review (`mailmate-learning::DefaultProposalReview`) adapters are injected at the edge.

use std::sync::Arc;

use mailmate_common::curator::{CuratorReport, CuratorRequest, ReviewDecision, ReviewOutcome};
use mailmate_common::error::{CuratorError, ReviewError};
use mailmate_common::proposal::AgentProposal;
use mailmate_ports::proposal_review::ProposalReview;
use mailmate_ports::rule_curator::RuleCurator;

use crate::Ports;

/// Runs curator passes over the rule system.
#[derive(Clone)]
pub struct CurationService {
    curator: Arc<dyn RuleCurator>,
}

impl CurationService {
    /// Assemble the service from the curator port.
    #[must_use]
    pub fn new(curator: Arc<dyn RuleCurator>) -> Self {
        Self { curator }
    }

    /// Assemble the service from the core's [`Ports`] bundle.
    #[must_use]
    pub fn from_ports(ports: &Ports) -> Self {
        Self::new(ports.rule_curator.clone())
    }

    /// Run a curator pass, returning the proposals it persisted (as `pending_review`) and the
    /// conflicts/observations it produced.
    ///
    /// # Errors
    /// [`CuratorError`] if the provider fails/returns an invalid response, or on a
    /// storage/rule-engine failure.
    pub async fn run(&self, request: CuratorRequest) -> Result<CuratorReport, CuratorError> {
        self.curator.curate(request).await
    }
}

/// The human review gate over agent proposals.
#[derive(Clone)]
pub struct ReviewService {
    review: Arc<dyn ProposalReview>,
}

impl ReviewService {
    /// Assemble the service from the review port.
    #[must_use]
    pub fn new(review: Arc<dyn ProposalReview>) -> Self {
        Self { review }
    }

    /// Assemble the service from the core's [`Ports`] bundle.
    #[must_use]
    pub fn from_ports(ports: &Ports) -> Self {
        Self::new(ports.proposal_review.clone())
    }

    /// The proposals awaiting a human decision, newest first.
    ///
    /// # Errors
    /// [`ReviewError::Storage`] on a backend failure.
    pub async fn queue(&self) -> Result<Vec<AgentProposal>, ReviewError> {
        self.review.pending().await
    }

    /// Apply a human's accept/reject decision. On acceptance the recommended rule is created
    /// in its recommended (non-active) status; on rejection nothing is created. Either way the
    /// decision is recorded.
    ///
    /// # Errors
    /// [`ReviewError`] if the proposal is unknown, already reviewed, missing a draft on an
    /// accept, or a backend failure occurs.
    pub async fn decide(&self, decision: ReviewDecision) -> Result<ReviewOutcome, ReviewError> {
        self.review.review(decision).await
    }
}
