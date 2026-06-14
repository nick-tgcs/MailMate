//! MailMate's decision-orchestration layer: the two pipelines that turn a message into a
//! classified, planned, and (downstream) guarded outcome.
//!
//! - [`CascadeClassifier`] implements the `ClassificationEngine` port — Pipeline 1, the
//!   three-tier cascade (deterministic rules → local Tier-2 model → LLM escalation) that
//!   answers *"what is this email"* while sending content to a model only on escalation.
//! - [`DefaultActionPlanner`] implements the `ActionPlanner` port — Pipeline 2, which runs
//!   the action rules over the classification and translates the winning effects into a
//!   candidate [`ActionPlan`](mailmate_common::action::ActionPlan) of untrusted
//!   `ProposedAction`s for the policy guard to evaluate.
//!
//! Both compose other **ports** (`RuleEngine`, `Tier2Classifier`, `AiProvider`) and the
//! `mailmate-ai` task layer; neither names a backend. The policy guard is deliberately a
//! separate, later step (a plan and its safety verdict never share one ladder), and the
//! planner runs **no model** — the provider's P2 influence is mediated entirely through the
//! classification it produced in P1 (determinism-first).

pub mod cascade;
pub mod context;
pub mod effects;
pub mod planner;

pub use cascade::CascadeClassifier;
pub use planner::DefaultActionPlanner;

/// Returns this crate's package name for smoke tests.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_available() {
        assert_eq!(super::crate_name(), "mailmate-planner");
    }
}
