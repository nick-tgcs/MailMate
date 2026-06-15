//! The follow-up scheduler port: the catch-up-on-launch drain. **Not a daemon** — the host
//! calls it on startup (catch-up sweep) and on a periodic tick while it is alive. It polls
//! the durable `workflow_instances.next_due_at` column (via the `(status, next_due_at)`
//! index), applies the coalescing/staleness guard, drives the existing drafter to produce a
//! review-required draft, and returns a [`DrainReport`] the host turns into frames.

use async_trait::async_trait;

use mailmate_common::error::WorkflowError;
use mailmate_common::time::Timestamp;
use mailmate_common::workflow::DrainReport;

/// Drains due follow-up steps into review-required drafts (catch-up-on-launch).
#[async_trait]
pub trait FollowUpScheduler: Send + Sync {
    /// Drain every due instance at `now`: apply the coalescing/staleness guard, emit at
    /// most one review-required draft per instance (coalescing the rest), move stale
    /// instances to `needs_attention`, and return the [`DrainReport`]. Idempotent enough to
    /// re-run: an instance moved to `awaiting_review`/`needs_attention` clears its
    /// `next_due_at` and is no longer selected.
    ///
    /// # Errors
    /// [`WorkflowError`] on a storage or drafting failure.
    async fn drain_due(&self, now: Timestamp) -> Result<DrainReport, WorkflowError>;

    /// Restart recovery. Because a drain only advances an instance after its draft is
    /// produced and persisted, there is no half-fired state to repair; recovery is the
    /// catch-up sweep itself (re-running [`drain_due`](Self::drain_due) at launch). This
    /// hook exists for symmetry with the documented contract and to let a future
    /// lease-based backend reclaim orphaned leases.
    ///
    /// # Errors
    /// [`WorkflowError`] on a storage failure.
    async fn recover(&self) -> Result<(), WorkflowError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn FollowUpScheduler) {}
        let _ = takes as fn(&dyn FollowUpScheduler);
    }
}
