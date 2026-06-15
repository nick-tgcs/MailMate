//! The shadow-mode **workflow** promotion report: scores a candidate cadence's fit from its
//! `workflow_shadow_outcomes` rows.
//!
//! This is honestly weaker than rule shadow precision. The true counterfactual — *would the
//! user have sent the drafted follow-up a shadow run never produced?* — is **unobservable**
//! (no draft exists to accept or reject in the shadow world). So the report measures an
//! observable proxy per would-fire step: did a reply arrive first (the step would have been
//! wasted), and did the user manually follow up near the would-fire time (the cadence
//! matches real behaviour). The aggregate is a *behavioural-alignment* estimate, not a
//! send-acceptance rate — and the report says so.

use mailmate_common::workflow::WorkflowShadowOutcome;

/// A cadence-fit estimate over a workflow's shadow outcomes. Every rate is over the
/// would-fire steps; `cadence_fit_score` is the manual-follow-up alignment **net of**
/// reply-pre-emption (a step the reply pre-empted is neither a hit nor counted against fit).
#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowPromotionReport {
    /// Total would-fire steps recorded in shadow.
    pub would_fire_steps: usize,
    /// Steps where a reply had already arrived (the draft would have been redundant).
    pub reply_pre_empted: usize,
    /// Steps the user manually followed up near (the cadence matched real behaviour).
    pub manual_followup_matched: usize,
    /// Manual-follow-up alignment over the steps a reply did **not** pre-empt, in `[0, 1]`.
    /// `None` when every step was pre-empted (no scoreable step).
    pub cadence_fit_score: Option<f64>,
    /// The honest caveat, always present so a reader cannot mistake this for a send rate.
    pub caveat: &'static str,
}

const CAVEAT: &str =
    "behavioural-alignment estimate (manual-followup fit net of reply pre-emption), \
     NOT a send-acceptance rate: the send counterfactual is unobservable in shadow";

/// Compute the promotion report for a workflow's shadow outcomes.
#[must_use]
pub fn workflow_promotion_report(rows: &[WorkflowShadowOutcome]) -> WorkflowPromotionReport {
    let would_fire_steps = rows.len();
    let reply_pre_empted = rows.iter().filter(|r| r.reply_before_fire).count();
    let manual_followup_matched = rows
        .iter()
        .filter(|r| r.matched_manual_followup_within_days.is_some())
        .count();

    // Score only the steps a reply did not pre-empt — a pre-empted step is not a cadence
    // failure (the reply made it moot), so it neither helps nor hurts the fit.
    let scoreable = would_fire_steps - reply_pre_empted;
    let cadence_fit_score = if scoreable == 0 {
        None
    } else {
        let matched_unpre_empted = rows
            .iter()
            .filter(|r| !r.reply_before_fire && r.matched_manual_followup_within_days.is_some())
            .count();
        Some(matched_unpre_empted as f64 / scoreable as f64)
    };

    WorkflowPromotionReport {
        would_fire_steps,
        reply_pre_empted,
        manual_followup_matched,
        cadence_fit_score,
        caveat: CAVEAT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::ids::{
        PipelineItemId, ThreadId, WorkflowDefId, WorkflowDefVersionId, WorkflowShadowOutcomeId,
    };
    use mailmate_common::time::Timestamp;

    fn outcome(reply_before: bool, matched: Option<i64>) -> WorkflowShadowOutcome {
        WorkflowShadowOutcome {
            id: WorkflowShadowOutcomeId::fresh(),
            workflow_id: WorkflowDefId::from("wfd_1"),
            workflow_version_id: WorkflowDefVersionId::from("wfdv_1"),
            pipeline_item_id: PipelineItemId::from("pli_1"),
            thread_id: ThreadId::from("thread_1"),
            step_index: 0,
            would_fire_at: Timestamp::now(),
            reply_before_fire: reply_before,
            matched_manual_followup_within_days: matched,
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn cadence_fit_excludes_reply_pre_empted_steps() {
        let rows = vec![
            outcome(false, Some(1)), // scoreable hit
            outcome(false, None),    // scoreable miss
            outcome(true, None),     // pre-empted (excluded)
            outcome(true, Some(2)),  // pre-empted (excluded, even though matched)
        ];
        let report = workflow_promotion_report(&rows);
        assert_eq!(report.would_fire_steps, 4);
        assert_eq!(report.reply_pre_empted, 2);
        assert_eq!(report.manual_followup_matched, 2);
        // Scoreable = 4 - 2 = 2; matched-and-not-pre-empted = 1 → 0.5.
        assert_eq!(report.cadence_fit_score, Some(0.5));
        assert!(report.caveat.contains("NOT a send-acceptance rate"));
    }

    #[test]
    fn all_pre_empted_yields_no_score() {
        let rows = vec![outcome(true, None), outcome(true, Some(1))];
        let report = workflow_promotion_report(&rows);
        assert_eq!(report.cadence_fit_score, None, "nothing scoreable");
    }

    #[test]
    fn empty_shadow_history_scores_nothing() {
        let report = workflow_promotion_report(&[]);
        assert_eq!(report.would_fire_steps, 0);
        assert_eq!(report.cadence_fit_score, None);
    }
}
