//! The pure follow-up cadence FSM: given a workflow version's content, an instance's
//! cursor, and `now`, decide what (if anything) is due — applying the catch-up, coalescing,
//! and staleness guard. No I/O, no ports, no clock: every function takes timestamps
//! explicitly so it is exhaustively unit-testable. This mirrors the rule evaluator's
//! purity (the engine adapter does the I/O around these decisions).

use mailmate_common::time::Timestamp;
use mailmate_common::workflow::{WorkflowInstanceStatus, WorkflowVersionContent};

/// What one due instance should do this drain pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DrainDecision {
    /// Fire `step_index` (the latest fresh due step), coalescing the earlier due steps in
    /// `coalesced_from` (recorded as `step_skipped_coalesced`, no draft of their own).
    Fire {
        /// The step to draft.
        step_index: i64,
        /// Earlier due steps collapsed into this one.
        coalesced_from: Vec<i64>,
    },
    /// Every due step is stale past the abandon horizon — surface a nudge, no draft.
    NeedsAttention {
        /// The due step indexes that were skipped.
        skipped_step_indexes: Vec<i64>,
    },
    /// Nothing is due (the row should not have been selected, or the cadence is exhausted).
    Nothing,
}

/// The absolute due time of the step at `step_index`, or `None` if there is no such step.
#[must_use]
pub fn due_time_for_step(
    content: &WorkflowVersionContent,
    anchor_at: Timestamp,
    step_index: i64,
) -> Option<Timestamp> {
    content
        .step(step_index)
        .map(|step| anchor_at.add_days(step.offset_days))
}

/// The due time of the lowest-index step at or after `from_index` (the next step to fire),
/// or `None` if the cadence is exhausted.
#[must_use]
pub fn next_due_after(
    content: &WorkflowVersionContent,
    anchor_at: Timestamp,
    from_index: i64,
) -> Option<Timestamp> {
    content
        .steps
        .iter()
        .filter(|s| s.step_index >= from_index)
        .min_by_key(|s| s.step_index)
        .map(|step| anchor_at.add_days(step.offset_days))
}

/// Decide what a due instance does at `now`, applying the catch-up + coalescing + staleness
/// guard. `current_step_index` is the next step the instance is waiting to fire.
#[must_use]
pub fn evaluate_due(
    content: &WorkflowVersionContent,
    anchor_at: Timestamp,
    current_step_index: i64,
    now: Timestamp,
) -> DrainDecision {
    let horizon = content.staleness.abandon_horizon_days;

    // The due set: every step at or after the cursor whose absolute due-time has passed.
    let mut due: Vec<(i64, bool)> = content
        .steps
        .iter()
        .filter(|s| s.step_index >= current_step_index)
        .filter_map(|s| {
            let due_at = anchor_at.add_days(s.offset_days);
            if due_at <= now {
                // Fresh if within the horizon; stale if overdue past it.
                let fresh = now.whole_days_since(due_at) <= horizon;
                Some((s.step_index, fresh))
            } else {
                None
            }
        })
        .collect();
    due.sort_by_key(|(idx, _)| *idx);

    if due.is_empty() {
        return DrainDecision::Nothing;
    }

    let fresh: Vec<i64> = due.iter().filter(|(_, f)| *f).map(|(i, _)| *i).collect();
    if fresh.is_empty() {
        // All due steps are stale past the horizon: a 30-day absence nudges, never nags.
        return DrainDecision::NeedsAttention {
            skipped_step_indexes: due.iter().map(|(i, _)| *i).collect(),
        };
    }

    // Fire the latest fresh step; coalesce every earlier due step into it. With coalescing
    // disabled, fire the *earliest* fresh step alone (one per drain pass).
    let latest = if content.staleness.coalesce {
        *fresh.iter().max().expect("fresh is non-empty")
    } else {
        *fresh.iter().min().expect("fresh is non-empty")
    };
    let coalesced_from: Vec<i64> = due
        .iter()
        .map(|(i, _)| *i)
        .filter(|i| *i < latest)
        .collect();
    DrainDecision::Fire {
        step_index: latest,
        coalesced_from,
    }
}

/// The instance state after a human resolves a surfaced review (`review_followup`): advance
/// the cursor past the fired step, re-arming on the next step (`active`) or completing if the
/// cadence is exhausted.
#[must_use]
pub fn advance_after_review(
    content: &WorkflowVersionContent,
    anchor_at: Timestamp,
    fired_step_index: i64,
) -> (WorkflowInstanceStatus, i64, Option<Timestamp>) {
    let next_index = fired_step_index + 1;
    match next_due_after(content, anchor_at, next_index) {
        Some(due) => (WorkflowInstanceStatus::Active, next_index, Some(due)),
        None => (WorkflowInstanceStatus::Completed, next_index, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::actor::Actor;
    use mailmate_common::rules::rule::RiskLevel;
    use mailmate_common::workflow::{ExitCondition, FollowUpStep, Staleness, WorkflowAnchor};

    fn content(coalesce: bool, horizon: i64) -> WorkflowVersionContent {
        WorkflowVersionContent {
            title: "t".to_owned(),
            description: "d".to_owned(),
            anchor: WorkflowAnchor::QuoteSentAt,
            enrollment_condition: None,
            steps: vec![step(0, 3), step(1, 7), step(2, 14)],
            exit_conditions: vec![ExitCondition::ReplyReceived],
            staleness: Staleness {
                coalesce,
                abandon_horizon_days: horizon,
            },
            risk_level: RiskLevel::Medium,
            change_reason: "seed".to_owned(),
            created_by: Actor::User,
        }
    }

    fn step(idx: i64, offset: i64) -> FollowUpStep {
        FollowUpStep {
            step_index: idx,
            offset_days: offset,
            draft_intent: "x".to_owned(),
            prompt_template_ref: None,
            forbidden_commitments: vec![],
        }
    }

    fn at(day: &str) -> Timestamp {
        Timestamp::parse_rfc3339(&format!("2026-06-{day}T00:00:00Z")).unwrap()
    }

    #[test]
    fn nothing_is_due_before_the_first_offset() {
        let c = content(true, 14);
        let anchor = at("01");
        // Day 2: step 0 (day 3) not yet due.
        assert_eq!(
            evaluate_due(&c, anchor, 0, at("02")),
            DrainDecision::Nothing
        );
    }

    #[test]
    fn a_single_fresh_step_fires_alone() {
        let c = content(true, 14);
        let anchor = at("01");
        // Day 4: step 0 (day 3) due, fresh; steps 1/2 future.
        assert_eq!(
            evaluate_due(&c, anchor, 0, at("04")),
            DrainDecision::Fire {
                step_index: 0,
                coalesced_from: vec![]
            }
        );
    }

    #[test]
    fn overdue_fresh_steps_coalesce_into_the_latest() {
        let c = content(true, 14);
        let anchor = at("01");
        // Day 19 with cursor at step 1: steps 1 (day 7→day 8 due) and 2 (day 14→day 15 due)
        // are both overdue. now=day19; step1 due day8 → 11 days late (<=14 fresh); step2 due
        // day15 → 4 days late (fresh). Coalesce → fire 2, skip 1.
        assert_eq!(
            evaluate_due(&c, anchor, 1, at("19")),
            DrainDecision::Fire {
                step_index: 2,
                coalesced_from: vec![1]
            }
        );
    }

    #[test]
    fn all_stale_past_the_horizon_needs_attention() {
        let c = content(true, 5); // tight 5-day horizon
        let anchor = at("01");
        // Day 28: step 0 (day3) 25d late, 1 (day7) 21d late, 2 (day14) 14d late — all > 5.
        assert_eq!(
            evaluate_due(&c, anchor, 0, at("28")),
            DrainDecision::NeedsAttention {
                skipped_step_indexes: vec![0, 1, 2]
            }
        );
    }

    #[test]
    fn a_fresh_step_among_stale_ones_still_fires() {
        let c = content(true, 5);
        let anchor = at("01");
        // Day 16: step0 (day3) 13d late stale, step1 (day7) 9d late stale, step2 (day14) 2d
        // late fresh. Fire 2, coalesce 0 and 1.
        assert_eq!(
            evaluate_due(&c, anchor, 0, at("16")),
            DrainDecision::Fire {
                step_index: 2,
                coalesced_from: vec![0, 1]
            }
        );
    }

    #[test]
    fn disabled_coalescing_fires_the_earliest_fresh_step() {
        let c = content(false, 30);
        let anchor = at("01");
        // Day 19 cursor 0: steps 0,1,2 all due and fresh (30d horizon). No coalesce → fire 0.
        assert_eq!(
            evaluate_due(&c, anchor, 0, at("19")),
            DrainDecision::Fire {
                step_index: 0,
                coalesced_from: vec![]
            }
        );
    }

    #[test]
    fn advance_re_arms_then_completes() {
        let c = content(true, 14);
        let anchor = at("01");
        let (status, idx, due) = advance_after_review(&c, anchor, 0);
        assert_eq!(status, WorkflowInstanceStatus::Active);
        assert_eq!(idx, 1);
        assert_eq!(due, Some(at("08"))); // day1 + 7
        let (status, idx, due) = advance_after_review(&c, anchor, 2);
        assert_eq!(status, WorkflowInstanceStatus::Completed);
        assert_eq!(idx, 3);
        assert_eq!(due, None);
    }

    #[test]
    fn next_due_after_finds_the_next_step() {
        let c = content(true, 14);
        let anchor = at("01");
        assert_eq!(next_due_after(&c, anchor, 0), Some(at("04")));
        assert_eq!(next_due_after(&c, anchor, 2), Some(at("15")));
        assert_eq!(next_due_after(&c, anchor, 3), None);
        assert_eq!(due_time_for_step(&c, anchor, 1), Some(at("08")));
        assert_eq!(due_time_for_step(&c, anchor, 9), None);
    }
}
