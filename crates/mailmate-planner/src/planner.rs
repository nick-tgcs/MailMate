//! The default action planner (`ActionPlanner`): Pipeline 2.
//!
//! It evaluates the **action** rules over `(message, classification, features)` — ranked by
//! the action hierarchy inside the rule engine — and translates the winning effects into a
//! candidate [`ActionPlan`]. It runs **no model**: the provider's influence on P2 is mediated
//! entirely through the `Classification` it produced in P1 (determinism-first). The candidate
//! plan still has to clear the policy guard before any action is applied — that is a separate,
//! later step, so a plan and its safety verdict never share one ladder.

use std::sync::Arc;

use async_trait::async_trait;

use mailmate_common::action::{ActionPlan, ProposedAction};
use mailmate_common::error::ActionPlanningError;
use mailmate_common::ids::RuleId;
use mailmate_common::planning::ActionPlanningInput;
use mailmate_ports::action_planner::ActionPlanner;
use mailmate_ports::rule_engine::RuleEngine;

use crate::context::action_context;
use crate::effects::translate_effect;

/// The default `ActionPlanner` adapter.
pub struct DefaultActionPlanner {
    action_rules: Arc<dyn RuleEngine>,
}

impl DefaultActionPlanner {
    /// A planner over an action-rule engine.
    #[must_use]
    pub fn new(action_rules: Arc<dyn RuleEngine>) -> Self {
        Self { action_rules }
    }
}

#[async_trait]
impl ActionPlanner for DefaultActionPlanner {
    async fn plan(&self, input: ActionPlanningInput) -> Result<ActionPlan, ActionPlanningError> {
        // Applying an action targets a resolved message id; planning requires a stored message.
        let message_id = input
            .message_id()
            .cloned()
            .ok_or(ActionPlanningError::MissingMessageId)?;

        let ctx = action_context(
            input.decision_id.clone(),
            &input.message,
            &input.classification,
            &input.features,
        );
        let result = self.action_rules.evaluate(ctx).await?;

        let mut actions = Vec::new();
        // Per-action provenance, kept length-matched with `actions`: one effect can translate to
        // several candidate actions (tag + move + junk), and each carries the authoring rule_id so
        // the apply path can stamp a rule's real fires. We extend by exactly the number of actions
        // this effect produced, so the sidecar stays aligned regardless of how many that is.
        let mut authored_by: Vec<Option<RuleId>> = Vec::new();
        for applied in &result.applied_effects {
            let before = actions.len();
            translate_effect(&applied.effect, &message_id, &mut actions);
            let produced = actions.len() - before;
            authored_by.extend(std::iter::repeat_n(Some(applied.rule_id.clone()), produced));
        }

        // A classification the cascade could not clear (no provider to escalate to) must be
        // surfaced for review rather than silently producing no action. No single rule authored
        // this fallback, so its provenance is `None`.
        if input.classification.needs_review
            && !actions
                .iter()
                .any(|action| matches!(action, ProposedAction::RequireReview { .. }))
        {
            actions.push(ProposedAction::RequireReview {
                target: format!("classification of message {message_id} needs review"),
            });
            authored_by.push(None);
        }

        debug_assert_eq!(
            actions.len(),
            authored_by.len(),
            "action provenance sidecar must stay length-matched with actions"
        );
        Ok(ActionPlan {
            decision_id: input.decision_id,
            message_id: Some(message_id),
            actions,
            authored_by,
        })
    }
}
