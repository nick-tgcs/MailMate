//! MailMate follow-up workflow adapters: the pure cadence FSM and the three engine
//! adapters that drive the existing review-required drafter on a *time* trigger.
//!
//! This crate is an **edge adapter**. It composes only PORTS (the workflow / instance /
//! pipeline-item repositories, the reply-drafter, audit, clock) and names no backend;
//! `mailmate-core` never depends on it. The follow-up feature is "not a third pipeline": a
//! due step drives the existing drafter to produce a `CreateDraft + RequireReview` draft
//! (review-required by construction, no send vocabulary anywhere), so
//! `never_auto_send_drafts` holds.
//!
//! - [`cadence`] — the pure FSM: catch-up + coalescing + staleness guard (no I/O).
//! - [`conflict`] — the pure containment conflict check.
//! - [`engine::DefaultWorkflowEngine`] — arm / detect conflicts / reschedule / resolve review.
//! - [`scheduler::DefaultFollowUpScheduler`] — the catch-up-on-launch drain → [`DrainReport`].
//! - [`exit::DefaultExitDetector`] — reply / won / lost / cancel exit handling.
//!
//! [`DrainReport`]: mailmate_common::workflow::DrainReport

pub mod cadence;
pub mod conflict;
pub mod engine;
pub mod exit;
pub mod scheduler;

pub use engine::DefaultWorkflowEngine;
pub use exit::DefaultExitDetector;
pub use scheduler::DefaultFollowUpScheduler;

/// Returns this crate's package name for smoke tests.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_available() {
        assert_eq!(super::crate_name(), "mailmate-workflow");
    }
}
