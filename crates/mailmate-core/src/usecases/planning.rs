//! The planning use-case: the two-pipeline orchestration that turns an arrived (or
//! follow-up-due) message into a classified, planned, and policy-guarded outcome.
//!
//! This is the async orchestration seam the architecture calls `app.rs`: it awaits the
//! classification engine and (for Tier-3) provider calls, then runs the deterministic
//! planning + policy steps. It names only **ports** — `FeatureExtractor`,
//! `ClassificationEngine`, `ActionPlanner`, `PolicyGuard` — so the cascade, the planner, and
//! the guard are all injected at the edge and the core never sees a concrete engine.

use std::sync::Arc;

use mailmate_common::action::GuardedActionPlan;
use mailmate_common::classification::{Classification, ClassificationInput};
use mailmate_common::error::MailMateError;
use mailmate_common::ids::DecisionId;
use mailmate_common::mail::MessageData;
use mailmate_common::planning::ActionPlanningInput;
use mailmate_common::policy::{PolicyContext, TriggerKind};
use mailmate_ports::action_planner::ActionPlanner;
use mailmate_ports::classification_engine::ClassificationEngine;
use mailmate_ports::feature_extractor::FeatureExtractor;
use mailmate_ports::policy_guard::PolicyGuard;

use crate::Ports;

/// The end-to-end outcome of the two pipelines for one message: the P1 verdict and the
/// P2 plan after the policy guard has partitioned it into allowed / review / blocked.
#[derive(Clone, Debug)]
pub struct PlanningOutcome {
    /// Pipeline 1's classification of the message.
    pub classification: Classification,
    /// Pipeline 2's candidate plan, after the policy guard.
    pub guarded_plan: GuardedActionPlan,
}

/// Runs `classify → plan → guard` over the injected ports.
#[derive(Clone)]
pub struct PlanningService {
    feature_extractor: Arc<dyn FeatureExtractor>,
    classifier: Arc<dyn ClassificationEngine>,
    planner: Arc<dyn ActionPlanner>,
    guard: Arc<dyn PolicyGuard>,
}

impl PlanningService {
    /// Assemble the service from its four ports.
    #[must_use]
    pub fn new(
        feature_extractor: Arc<dyn FeatureExtractor>,
        classifier: Arc<dyn ClassificationEngine>,
        planner: Arc<dyn ActionPlanner>,
        guard: Arc<dyn PolicyGuard>,
    ) -> Self {
        Self {
            feature_extractor,
            classifier,
            planner,
            guard,
        }
    }

    /// Assemble the service from the core's [`Ports`] dependency bundle.
    #[must_use]
    pub fn from_ports(ports: &Ports) -> Self {
        Self::new(
            ports.feature_extractor.clone(),
            ports.classification_engine.clone(),
            ports.action_planner.clone(),
            ports.policy_guard.clone(),
        )
    }

    /// Handle a newly-arrived message (`NewMail` trigger).
    ///
    /// # Errors
    /// Propagates any classification, planning, or policy failure as a [`MailMateError`].
    pub async fn handle_new_mail(
        &self,
        message: MessageData,
    ) -> Result<PlanningOutcome, MailMateError> {
        self.handle_message(message, TriggerKind::NewMail).await
    }

    /// Handle a message under an explicit trigger (new mail, or a follow-up coming due).
    ///
    /// # Errors
    /// Propagates any classification, planning, or policy failure as a [`MailMateError`].
    pub async fn handle_message(
        &self,
        message: MessageData,
        trigger: TriggerKind,
    ) -> Result<PlanningOutcome, MailMateError> {
        let message_id = message.id.clone();

        // Pipeline 1 — classify. Feature extraction is pure; the cascade sends content to a
        // model only on escalation.
        let features = self.feature_extractor.extract(&message);
        let classification = self
            .classifier
            .classify(ClassificationInput {
                decision_id: DecisionId::fresh(),
                message: message.clone(),
                features: features.clone(),
            })
            .await?;

        // Pipeline 2 — plan, then guard. The candidate plan and its safety verdict are
        // produced by two separate steps (never one ladder).
        let plan_input =
            ActionPlanningInput::new(trigger, message, classification.clone(), features);
        let plan = self.planner.plan(plan_input).await?;

        let policy_context = PolicyContext {
            message_id,
            categories: classification.policy_categories(),
            is_manual_user_override: false,
            explicit_move_allowance: false,
            trigger,
        };
        let guarded_plan = self
            .guard
            .evaluate_action_plan(policy_context, plan)
            .await?;

        Ok(PlanningOutcome {
            classification,
            guarded_plan,
        })
    }
}
