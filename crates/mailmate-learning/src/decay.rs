//! Rule decay → human-gated **retire** proposals (Phase 7, 7b).
//!
//! A rule that keeps getting undone is doing more harm than good. This module turns that
//! track record — read from the per-rule provenance the audit timeline stamps — into a
//! [`ProposalKind::RetireRule`] proposal the *human* decides on. Like every other proposal,
//! it only ever surfaces: the engine never auto-retires (the materialization-is-human-only
//! invariant).
//!
//! **Count vs rate, honestly.** The strongest decay signal is the undo *rate* (undos ÷ fires).
//! That needs a trustworthy per-rule *fires* count, which the apply path now provides: an
//! auto-applied action stamps `action_applied` with the authoring `rule_id` (the planner threads
//! it from `AppliedEffect::rule_id` through the guard, and the extension echoes the same id back on
//! Undo so `action_undone` keys on it too). So when a rule has a real fires denominator the **rate**
//! governs — a high-volume rule undone a handful of times out of hundreds of fires is *not* retired
//! — and the **count** is the fallback only for a rule with no recorded fires yet (a freshly
//! activated rule, or one whose fires predate the stamping). No fabricated rate either way.

use mailmate_common::ids::ProposalId;
use mailmate_common::proposal::{AgentProposal, ProposalKind, ProposalStatus};
use mailmate_common::rules::rule::{EvaluatableRule, RiskLevel, RuleStatus};
use mailmate_common::time::Timestamp;

/// When a live rule's track record warrants surfacing a retire proposal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecayThresholds {
    /// Minimum undos before a retire is proposed on the undo *count* alone — the fallback used for
    /// a rule with no recorded fires denominator yet (a freshly activated rule, or fires predating
    /// the per-rule stamping).
    pub min_undos: usize,
    /// Minimum per-rule fires before an undo *rate* is trustworthy enough to govern. Below this
    /// (or with no fires count at all) the count fallback applies.
    pub min_fires_for_rate: usize,
    /// The undo-rate (undos ÷ fires) at or above which a rule is judged to be misbehaving.
    pub undo_rate_floor: f64,
    /// Whole days of inactivity after which a live rule is judged **stale** — it hasn't fired in
    /// this long (or has never fired since it went active). A stale rule is dead weight the user
    /// may want to retire; like every decay signal it only *proposes*, never auto-retires.
    pub max_idle_days: i64,
}

impl Default for DecayThresholds {
    fn default() -> Self {
        Self {
            min_undos: 3,
            min_fires_for_rate: 8,
            undo_rate_floor: 0.3,
            max_idle_days: 60,
        }
    }
}

/// The evidence that a live rule has gone **stale** — it has done nothing useful for a long time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StaleVerdict {
    /// Whole days since the rule last did anything — since its last fire, or (if it has never
    /// fired) since it went active.
    pub idle_days: i64,
    /// `true` when the rule has *never* fired since activation (dead on arrival); `false` when it
    /// used to fire but has since gone quiet.
    pub never_fired: bool,
}

/// Assess whether a live rule has gone stale: no fire in `max_idle_days`, measured from its last
/// fire (`last_fire`) or — if it has never fired — from when it went active (`activated_at`).
///
/// Returns `None` (keep the rule) when it is still active recently, OR when neither reference
/// instant is known: with no activation timestamp and no fires we cannot tell a brand-new rule from
/// a long-dead one, so we **degrade rather than lie** and propose nothing. A clock skew that makes
/// `now` precede the reference yields a negative idle count, which is below the threshold — also a
/// safe "not stale". A `Some` verdict only *surfaces a proposal*; it never retires anything.
#[must_use]
pub fn assess_staleness(
    last_fire: Option<Timestamp>,
    activated_at: Option<Timestamp>,
    now: Timestamp,
    max_idle_days: i64,
) -> Option<StaleVerdict> {
    let (reference, never_fired) = match (last_fire, activated_at) {
        // It has fired before — measure idleness from the last fire.
        (Some(lf), _) => (lf, false),
        // It has never fired — measure from when it went active (it has had its whole active life
        // to fire and hasn't).
        (None, Some(act)) => (act, true),
        // No reference instant at all: cannot judge staleness honestly.
        (None, None) => return None,
    };
    let idle_days = now.whole_days_since(reference);
    (idle_days >= max_idle_days).then_some(StaleVerdict {
        idle_days,
        never_fired,
    })
}

/// The evidence that a rule's track record warrants a retire proposal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecayVerdict {
    /// How many of this rule's auto-applied actions the user undid.
    pub undos: usize,
    /// How many times the rule fired, when a per-rule fires count is available (else `None` —
    /// the fires provenance is not stamped yet).
    pub fires: Option<usize>,
    /// The undo-rate (undos ÷ fires), when fires are known and non-zero.
    pub undo_rate: Option<f64>,
}

/// Assess whether a rule's track record warrants a retire proposal.
///
/// With a trustworthy per-rule fires count (`fires >= min_fires_for_rate`) the **rate** governs:
/// retire only when undos ÷ fires ≥ `undo_rate_floor` — so a high-volume rule undone a few times
/// out of many fires is kept. Without that denominator (the case today), the **count** governs:
/// retire when undos ≥ `min_undos`. Returns `None` (keep the rule) otherwise. A `Some` verdict
/// only *surfaces a proposal*; it never retires anything.
#[must_use]
pub fn assess_decay(
    undos: usize,
    fires: Option<usize>,
    thresholds: DecayThresholds,
) -> Option<DecayVerdict> {
    let undo_rate = fires.filter(|&f| f > 0).map(|f| undos as f64 / f as f64);
    let triggered = match fires {
        // A real denominator: the rate governs, the raw count does not.
        Some(f) if f >= thresholds.min_fires_for_rate => {
            undo_rate.is_some_and(|r| r >= thresholds.undo_rate_floor)
        }
        // No trustworthy denominator: fall back to the undo count.
        _ => undos >= thresholds.min_undos,
    };
    triggered.then_some(DecayVerdict {
        undos,
        fires,
        undo_rate,
    })
}

/// Build a human-gated **retire** proposal for a decayed rule. It targets the existing rule (no
/// new draft), recommends [`Retired`](RuleStatus::Retired) — exactly the status the review
/// handler transitions the target to on acceptance, so the card never promises a different
/// outcome — and routes to [`PendingReview`](ProposalStatus::PendingReview): the engine never
/// auto-retires. The rationale states exactly the signal it has (a rate when fires are known, a
/// count otherwise), never a fabricated rate.
#[must_use]
pub fn retire_proposal(
    rule: &EvaluatableRule,
    verdict: DecayVerdict,
    source: &str,
) -> AgentProposal {
    let rationale = match (verdict.fires, verdict.undo_rate) {
        (Some(fires), Some(rate)) => format!(
            "This rule fired {fires} times and you undid {} of them ({:.0}% undo rate). Retire it?",
            verdict.undos,
            rate * 100.0
        ),
        _ => format!(
            "You undid this rule's action {} times. Retire it?",
            verdict.undos
        ),
    };
    AgentProposal {
        id: ProposalId::fresh(),
        proposal_type: ProposalKind::RetireRule,
        status: ProposalStatus::PendingReview,
        title: "Retire a rule you keep undoing".to_owned(),
        rationale,
        risk_level: RiskLevel::Medium,
        recommended_status: RuleStatus::Retired,
        rule_draft: None,
        target_rule_kind: Some(rule.kind),
        target_rule_id: Some(rule.rule_id.clone()),
        workflow_draft: None,
        target_workflow_id: None,
        evidence_refs: vec![],
        back_test: None,
        conflicts: Vec::new(),
        source_provider: source.to_owned(),
        created_at: Timestamp::now(),
        reviewed_at: None,
    }
}

/// Build a human-gated **retire** proposal for a *stale* rule. Same shape as [`retire_proposal`]
/// (targets the existing rule, recommends [`Retired`](RuleStatus::Retired), routes to
/// [`PendingReview`](ProposalStatus::PendingReview) — never auto-retires), but the rationale states
/// the staleness signal: how long it has been idle, and whether it ever fired at all. A stale rule
/// is low-risk to retire (it isn't doing anything), so the risk level is `Low`.
#[must_use]
pub fn stale_retire_proposal(
    rule: &EvaluatableRule,
    verdict: StaleVerdict,
    source: &str,
) -> AgentProposal {
    let rationale = if verdict.never_fired {
        format!(
            "This rule has been active for {} days and has never fired. Retire it?",
            verdict.idle_days
        )
    } else {
        format!(
            "This rule hasn't fired in {} days. Retire it?",
            verdict.idle_days
        )
    };
    AgentProposal {
        id: ProposalId::fresh(),
        proposal_type: ProposalKind::RetireRule,
        status: ProposalStatus::PendingReview,
        title: "Retire a rule that's gone quiet".to_owned(),
        rationale,
        risk_level: RiskLevel::Low,
        recommended_status: RuleStatus::Retired,
        rule_draft: None,
        target_rule_kind: Some(rule.kind),
        target_rule_id: Some(rule.rule_id.clone()),
        workflow_draft: None,
        target_workflow_id: None,
        evidence_refs: vec![],
        back_test: None,
        conflicts: Vec::new(),
        source_provider: source.to_owned(),
        created_at: Timestamp::now(),
        reviewed_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::ids::{RuleId, RuleVersionId};
    use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
    use mailmate_common::rules::effect::RuleEffect;
    use mailmate_common::rules::rule::{HierarchyBand, RuleKind, RuleScope, RuleVersion};

    fn rule(id: &str) -> EvaluatableRule {
        EvaluatableRule {
            rule_id: RuleId::from(id),
            kind: RuleKind::Action,
            scope: RuleScope::Domain,
            band: HierarchyBand::LearnedActive,
            status: RuleStatus::Active,
            version: RuleVersion {
                id: RuleVersionId::from("rv_1"),
                version_number: 1,
                condition: Condition::Predicate(Predicate {
                    field: "sender_domain".to_owned(),
                    op: Operator::Eq,
                    value: FieldValue::Text("x.com".to_owned()),
                }),
                effect: RuleEffect {
                    move_to: Some("Spam".to_owned()),
                    ..RuleEffect::new()
                },
                risk_level: RiskLevel::Low,
            },
        }
    }

    #[test]
    fn undo_count_triggers_when_no_fires_denominator_exists() {
        // The case today: fires aren't provenance-stamped per rule, so the count is the signal.
        let v = assess_decay(3, None, DecayThresholds::default()).expect("3 undos crosses the bar");
        assert_eq!(v.undos, 3);
        assert_eq!(v.fires, None);
        assert_eq!(v.undo_rate, None, "no denominator → no fabricated rate");
    }

    #[test]
    fn a_low_undo_count_keeps_the_rule() {
        assert_eq!(assess_decay(2, None, DecayThresholds::default()), None);
    }

    #[test]
    fn with_a_real_fires_count_the_rate_governs_not_the_raw_count() {
        let t = DecayThresholds::default();
        // A high-volume rule undone 3 times out of 200 fires is GOOD — not retired despite the
        // count crossing min_undos: the rate (0.015) is well under the floor.
        assert_eq!(assess_decay(3, Some(200), t), None);
        // A rule undone 4 of 10 fires (rate 0.4 ≥ 0.3) IS surfaced, and the rate is reported.
        let v = assess_decay(4, Some(10), t).expect("a 40% undo rate crosses the bar");
        assert_eq!(v.fires, Some(10));
        assert_eq!(v.undo_rate, Some(0.4));
    }

    #[test]
    fn retire_proposal_targets_the_rule_and_is_human_gated() {
        let r = rule("rule_spam");
        let v = assess_decay(5, None, DecayThresholds::default()).unwrap();
        let p = retire_proposal(&r, v, "learning-engine");
        assert_eq!(p.proposal_type, ProposalKind::RetireRule);
        assert_eq!(
            p.status,
            ProposalStatus::PendingReview,
            "human-gated, never auto"
        );
        assert_eq!(p.target_rule_id.as_ref(), Some(&RuleId::from("rule_spam")));
        assert_eq!(p.target_rule_kind, Some(RuleKind::Action));
        assert!(
            p.rule_draft.is_none(),
            "a retire references the rule, carries no new draft"
        );
        // Recommends exactly what acceptance does — the review handler retires the target.
        assert_eq!(p.recommended_status, RuleStatus::Retired);
        assert!(p.rationale.contains("5 times"), "{}", p.rationale);
    }

    #[test]
    fn the_rate_rationale_states_real_numbers_when_fires_are_known() {
        let r = rule("rule_spam");
        let v = assess_decay(4, Some(10), DecayThresholds::default()).unwrap();
        let p = retire_proposal(&r, v, "learning-engine");
        assert!(p.rationale.contains("fired 10 times"), "{}", p.rationale);
        assert!(p.rationale.contains("40% undo rate"), "{}", p.rationale);
    }

    #[test]
    fn a_rule_quiet_past_the_idle_window_is_stale_measured_from_its_last_fire() {
        let now = Timestamp::now();
        let max_idle = DecayThresholds::default().max_idle_days;
        // Last fired (max_idle + 5) days ago → idle past the window → stale, and it DID fire before.
        let last_fire = now.add_days(-(max_idle + 5));
        let v =
            assess_staleness(Some(last_fire), None, now, max_idle).expect("idle past the window");
        assert!(!v.never_fired, "it used to fire, then went quiet");
        assert_eq!(v.idle_days, max_idle + 5);
    }

    #[test]
    fn a_recently_fired_rule_is_not_stale() {
        let now = Timestamp::now();
        let max_idle = DecayThresholds::default().max_idle_days;
        // Fired yesterday → well within the window → not stale, even with an old activation.
        let activated = now.add_days(-365);
        assert_eq!(
            assess_staleness(Some(now.add_days(-1)), Some(activated), now, max_idle),
            None
        );
    }

    #[test]
    fn a_never_fired_rule_is_stale_only_after_the_window_since_activation() {
        let now = Timestamp::now();
        let max_idle = DecayThresholds::default().max_idle_days;
        // Active long ago, never fired → stale (dead on arrival).
        let v = assess_staleness(None, Some(now.add_days(-(max_idle + 1))), now, max_idle)
            .expect("never fired, active past the window");
        assert!(v.never_fired);
        // Freshly activated, never fired → NOT stale yet (it hasn't had its chance).
        assert_eq!(
            assess_staleness(None, Some(now.add_days(-1)), now, max_idle),
            None
        );
    }

    #[test]
    fn staleness_degrades_to_none_without_any_reference_instant() {
        // No last fire and no activation timestamp: we cannot tell new from long-dead, so propose
        // nothing rather than fabricate an idle age.
        assert_eq!(assess_staleness(None, None, Timestamp::now(), 60), None);
    }

    #[test]
    fn the_stale_proposal_is_human_gated_and_states_the_idle_signal() {
        let r = rule("rule_quiet");
        let v = StaleVerdict {
            idle_days: 90,
            never_fired: false,
        };
        let p = stale_retire_proposal(&r, v, "learning-engine");
        assert_eq!(p.proposal_type, ProposalKind::RetireRule);
        assert_eq!(
            p.status,
            ProposalStatus::PendingReview,
            "human-gated, never auto"
        );
        assert_eq!(p.recommended_status, RuleStatus::Retired);
        assert_eq!(p.target_rule_id.as_ref(), Some(&RuleId::from("rule_quiet")));
        assert!(
            p.rationale.contains("hasn't fired in 90 days"),
            "{}",
            p.rationale
        );

        let never = stale_retire_proposal(
            &r,
            StaleVerdict {
                idle_days: 75,
                never_fired: true,
            },
            "learning-engine",
        );
        assert!(
            never.rationale.contains("never fired"),
            "{}",
            never.rationale
        );
    }
}
