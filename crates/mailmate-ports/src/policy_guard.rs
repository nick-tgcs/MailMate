//! The policy-guard port: the hard-safety chokepoint every action plan passes through.
//!
//! The guard is deterministic domain logic, but — per the universal ports-and-adapters
//! law — it sits behind a port so the core names only this trait, never a concrete guard.
//! The default adapter (`mailmate-policy::HardPolicyGuard`) implements the seven hard
//! policies; a test mock can stand in for it. The action planner (Phase 6) is constructed
//! so its candidate plan is *always* routed through this trait before any action is
//! applied.

use async_trait::async_trait;

use mailmate_common::action::{ActionPlan, GuardedActionPlan};
use mailmate_common::error::PolicyError;
use mailmate_common::policy::PolicyContext;

/// Evaluates candidate action plans against MailMate's hard safety policies.
#[async_trait]
pub trait PolicyGuard: Send + Sync {
    /// Partition a candidate plan into allowed / review-required / blocked actions,
    /// recording which policy decided each.
    ///
    /// # Errors
    /// [`PolicyError`] if the context is missing a signal a policy needs, or on an
    /// adapter-level failure.
    async fn evaluate_action_plan(
        &self,
        context: PolicyContext,
        plan: ActionPlan,
    ) -> Result<GuardedActionPlan, PolicyError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn PolicyGuard) {}
        let _ = takes as fn(&dyn PolicyGuard);
    }
}
