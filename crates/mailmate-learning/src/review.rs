//! [`DefaultProposalReview`]: the [`ProposalReview`] port — applying a human's accept/reject
//! decision to an agent proposal.
//!
//! This is where "require human review for activation of risky rules" becomes a mechanism:
//! a proposal's recommended rule is created **only** here, on acceptance, and only into the
//! proposal's recommended status (`shadow_mode`/`pending_human_review`) — **never `active`**.
//! No path through this adapter activates a rule, so a risky rule cannot go live without a
//! further, separate human action. Every decision is recorded to `rule_proposal_feedback`
//! (so the curator loop is learnable) and to the audit timeline.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use mailmate_common::actor::Actor;
use mailmate_common::audit::{event_type, AuditEntry};
use mailmate_common::curator::{ReviewDecision, ReviewOutcome};
use mailmate_common::error::ReviewError;
use mailmate_common::feedback::{PinnedVersions, RuleProposalFeedback, RuleProposalFeedbackRow};
use mailmate_common::ids::RuleId;
use mailmate_common::proposal::{AgentProposal, ProposalKind, ProposalStatus};
use mailmate_common::rules::rule::{
    HierarchyBand, NewRule, NewRuleVersion, RuleStatus, RuleVersionContent,
};
use mailmate_common::time::Timestamp;
use mailmate_common::workflow::{NewWorkflowDefVersion, NewWorkflowDefinition};
use mailmate_ports::proposal_review::ProposalReview;
use mailmate_ports::storage::audit::AuditRepository;
use mailmate_ports::storage::feedback::FeedbackRepository;
use mailmate_ports::storage::proposals::ProposalRepository;
use mailmate_ports::storage::rules::RuleRepository;
use mailmate_ports::storage::workflows::WorkflowRepository;

/// The default proposal-review adapter, composing the proposal store, the rule store, the
/// `rule_proposal_feedback` table, and the audit timeline.
pub struct DefaultProposalReview {
    proposals: Arc<dyn ProposalRepository>,
    rules: Arc<dyn RuleRepository>,
    workflows: Arc<dyn WorkflowRepository>,
    feedback: Arc<dyn FeedbackRepository<RuleProposalFeedback>>,
    audit: Arc<dyn AuditRepository>,
}

impl DefaultProposalReview {
    /// Build a review adapter over the given stores.
    #[must_use]
    pub fn new(
        proposals: Arc<dyn ProposalRepository>,
        rules: Arc<dyn RuleRepository>,
        workflows: Arc<dyn WorkflowRepository>,
        feedback: Arc<dyn FeedbackRepository<RuleProposalFeedback>>,
        audit: Arc<dyn AuditRepository>,
    ) -> Self {
        Self {
            proposals,
            rules,
            workflows,
            feedback,
            audit,
        }
    }

    /// Materialize the rule an accepted proposal recommends, in its recommended (non-active)
    /// status. Returns the new rule id for a `new_rule` acceptance; `None` for kinds that
    /// mutate or reference an existing rule (refine/retire), or that need a manual multi-rule
    /// restructure (merge/split).
    async fn materialize(
        &self,
        proposal: &AgentProposal,
        now: Timestamp,
    ) -> Result<Option<RuleId>, ReviewError> {
        match proposal.proposal_type {
            ProposalKind::NewRule => {
                let draft = proposal
                    .rule_draft
                    .as_ref()
                    .ok_or_else(|| ReviewError::MissingDraft(proposal.id.to_string()))?;
                let new_rule = NewRule {
                    stable_name: format!("curated-{}", proposal.id),
                    kind: draft.kind,
                    scope: draft.scope,
                    // Agent-proposed rules enter the shadow band; a later human activation can
                    // promote them. They are NEVER created active.
                    band: HierarchyBand::AgentShadow,
                    created_by: Actor::User,
                    initial_version: RuleVersionContent {
                        title: proposal.title.clone(),
                        description: proposal.rationale.clone(),
                        condition: draft.condition.clone(),
                        effect: draft.effect.clone(),
                        priority: 0,
                        confidence_threshold: None,
                        risk_level: proposal.risk_level,
                        change_reason: format!("accepted from proposal {}", proposal.id),
                        created_by: Actor::User,
                    },
                };
                let rule_id = self.rules.save_rule_draft(new_rule).await?;
                self.rules
                    .update_rule_status(&rule_id, draft.kind, proposal.recommended_status)
                    .await?;
                self.append_status_audit(proposal, &rule_id, draft.kind, now)
                    .await?;
                Ok(Some(rule_id))
            }
            ProposalKind::RefineRule => {
                // A refine with a draft appends a new version to the target and re-points it;
                // without a draft it is a manual edit we only record.
                if let (Some(target), Some(kind), Some(draft)) = (
                    proposal.target_rule_id.as_ref(),
                    proposal.target_rule_kind,
                    proposal.rule_draft.as_ref(),
                ) {
                    self.rules
                        .create_rule_version(NewRuleVersion {
                            rule_id: target.clone(),
                            kind,
                            content: RuleVersionContent {
                                title: proposal.title.clone(),
                                description: proposal.rationale.clone(),
                                condition: draft.condition.clone(),
                                effect: draft.effect.clone(),
                                priority: 0,
                                confidence_threshold: None,
                                risk_level: proposal.risk_level,
                                change_reason: format!("refined via proposal {}", proposal.id),
                                created_by: Actor::User,
                            },
                        })
                        .await?;
                    self.append_status_audit(proposal, target, kind, now)
                        .await?;
                }
                Ok(None)
            }
            ProposalKind::RetireRule => {
                if let (Some(target), Some(kind)) =
                    (proposal.target_rule_id.as_ref(), proposal.target_rule_kind)
                {
                    self.rules
                        .update_rule_status(
                            target,
                            kind,
                            mailmate_common::rules::rule::RuleStatus::Retired,
                        )
                        .await?;
                    self.append_status_audit(proposal, target, kind, now)
                        .await?;
                }
                Ok(None)
            }
            // Merge/split need a human-specified multi-rule restructure that a single-target
            // proposal cannot express; acceptance records the decision without auto-editing.
            ProposalKind::MergeRules | ProposalKind::SplitRule => Ok(None),
            ProposalKind::NewWorkflow => {
                // Review is the only workflow-materialization path: it creates the definition
                // in its recommended (shadow/pending) status — NEVER active.
                let draft = proposal
                    .workflow_draft
                    .as_ref()
                    .ok_or_else(|| ReviewError::MissingDraft(proposal.id.to_string()))?;
                let new_def = NewWorkflowDefinition {
                    stable_name: format!("curated-wf-{}", proposal.id),
                    scope: draft.scope,
                    applies_to_item_type: draft.applies_to_item_type,
                    created_by: Actor::User,
                    initial_version: draft.clone().into_version_content(
                        proposal.title.clone(),
                        proposal.rationale.clone(),
                        format!("accepted from proposal {}", proposal.id),
                        Actor::User,
                    ),
                };
                let workflow_id = self.workflows.save_definition_draft(new_def).await?;
                self.workflows
                    .update_status(&workflow_id, proposal.recommended_status)
                    .await?;
                self.append_workflow_audit(proposal, workflow_id.as_str())
                    .await?;
                Ok(None)
            }
            ProposalKind::RefineWorkflowCadence | ProposalKind::RefineWorkflowStopCondition => {
                // A refine with a draft appends a new immutable version to the target workflow.
                if let (Some(target), Some(draft)) = (
                    proposal.target_workflow_id.as_ref(),
                    proposal.workflow_draft.as_ref(),
                ) {
                    self.workflows
                        .create_version(NewWorkflowDefVersion {
                            workflow_id: target.clone(),
                            content: draft.clone().into_version_content(
                                proposal.title.clone(),
                                proposal.rationale.clone(),
                                format!("refined via proposal {}", proposal.id),
                                Actor::User,
                            ),
                        })
                        .await?;
                    self.append_workflow_audit(proposal, target.as_str())
                        .await?;
                }
                Ok(None)
            }
            ProposalKind::RetireWorkflow => {
                if let Some(target) = proposal.target_workflow_id.as_ref() {
                    self.workflows
                        .update_status(target, RuleStatus::Retired)
                        .await?;
                    self.append_workflow_audit(proposal, target.as_str())
                        .await?;
                }
                Ok(None)
            }
            // Enrollment is a user action on a pipeline item, not a rule/workflow mutation;
            // acceptance records the decision (below) without auto-enrolling.
            ProposalKind::SuggestEnrollment => Ok(None),
        }
    }

    /// Audit a workflow-status change a review materialized (the workflow analogue of
    /// [`append_status_audit`](Self::append_status_audit), which is rule-id-typed).
    async fn append_workflow_audit(
        &self,
        proposal: &AgentProposal,
        workflow_id: &str,
    ) -> Result<(), ReviewError> {
        let entry = AuditEntry::new("workflow_status_changed", Actor::User)
            .with_proposal(proposal.id.clone())
            .with_payload(json!({
                "workflow_id": workflow_id,
                "to": proposal.recommended_status.as_str(),
                "proposal_type": proposal.proposal_type.as_str(),
            }));
        self.audit.append(entry).await?;
        Ok(())
    }

    async fn append_status_audit(
        &self,
        proposal: &AgentProposal,
        rule_id: &RuleId,
        kind: mailmate_common::rules::rule::RuleKind,
        _now: Timestamp,
    ) -> Result<(), ReviewError> {
        let entry = AuditEntry::new(event_type::RULE_STATUS_CHANGED, Actor::User)
            .with_rule(kind, rule_id.clone())
            .with_proposal(proposal.id.clone())
            .with_payload(json!({
                "to": proposal.recommended_status.as_str(),
                "proposal_type": proposal.proposal_type.as_str(),
            }));
        self.audit.append(entry).await?;
        Ok(())
    }
}

#[async_trait]
impl ProposalReview for DefaultProposalReview {
    async fn pending(&self) -> Result<Vec<AgentProposal>, ReviewError> {
        Ok(self
            .proposals
            .list_by_status(ProposalStatus::PendingReview)
            .await?)
    }

    async fn review(&self, decision: ReviewDecision) -> Result<ReviewOutcome, ReviewError> {
        let proposal = self
            .proposals
            .get(&decision.proposal_id)
            .await?
            .ok_or_else(|| ReviewError::NotFound(decision.proposal_id.to_string()))?;
        if proposal.status.is_terminal() {
            return Err(ReviewError::AlreadyReviewed(proposal.id.to_string()));
        }

        let now = Timestamp::now();
        let (new_status, created_rule_id) = if decision.outcome.is_acceptance() {
            let rule_id = self.materialize(&proposal, now).await?;
            (ProposalStatus::Accepted, rule_id)
        } else {
            (ProposalStatus::Rejected, None)
        };

        self.proposals
            .set_status(&proposal.id, new_status, Some(now))
            .await?;

        let feedback_row = RuleProposalFeedbackRow {
            id: RuleProposalFeedback::fresh_id(),
            proposal_id: proposal.id.clone(),
            pinned_versions: PinnedVersions::default(),
            outcome: decision.outcome,
            human_reason_code: decision.human_reason_code,
            human_reason_text: decision.human_reason_text,
            polarity: decision.outcome.polarity(),
            created_at: now,
        };
        let feedback_id = self.feedback.append(feedback_row).await?;

        let entry = AuditEntry::new(event_type::PROPOSAL_REVIEWED, Actor::User)
            .with_proposal(proposal.id.clone())
            .with_payload(json!({
                "outcome": decision.outcome.as_str(),
                "new_status": new_status.as_str(),
                "created_rule_id": created_rule_id.as_ref().map(RuleId::as_str),
            }));
        self.audit.append(entry).await?;

        Ok(ReviewOutcome {
            proposal_id: proposal.id,
            new_status,
            created_rule_id,
            feedback_id,
        })
    }
}
