//! The action-planner port: Pipeline 2's "what should we do about it" candidate plan.
//!
//! The planner runs the action rules over `(message, classification, features)`, ranked by
//! the action hierarchy, and translates the winning effects into a candidate
//! [`ActionPlan`](mailmate_common::action::ActionPlan) of *untrusted* `ProposedAction`s. It
//! does **not** guard them — the policy guard is a separate, later step (so a plan and its
//! safety verdict never share one ladder). The default adapter
//! (`mailmate-planner::DefaultActionPlanner`) runs no model: the provider's contribution to
//! P2 is mediated entirely through the `Classification` it produced in P1 (determinism-first).

use async_trait::async_trait;

use mailmate_common::action::ActionPlan;
use mailmate_common::error::ActionPlanningError;
use mailmate_common::planning::ActionPlanningInput;

/// Plans candidate actions for a classified message — Pipeline 2 of the two-pipeline flow.
#[async_trait]
pub trait ActionPlanner: Send + Sync {
    /// Produce the candidate [`ActionPlan`] for `input`. The plan still has to pass the
    /// policy guard before any action is applied.
    ///
    /// # Errors
    /// [`ActionPlanningError`] on a rule-evaluation failure or a message with no resolved id.
    async fn plan(&self, input: ActionPlanningInput) -> Result<ActionPlan, ActionPlanningError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn ActionPlanner) {}
        let _ = takes as fn(&dyn ActionPlanner);
    }
}
