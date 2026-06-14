//! Outcome monitoring: derive a [`RuleOutcome`] for a rule from the feedback rows that
//! name it and the shadow firings it produced.
//!
//! This is the *view* the architecture insists rule performance must be — computed in Rust
//! over plain row fetches (the honest, engine-portable fallback to a SQL view). It stores
//! nothing, so it cannot drift from or duplicate its sources.

use mailmate_common::feedback::{FeedbackPolarity, FilingFeedbackRow};
use mailmate_common::ids::RuleId;
use mailmate_common::outcome::RuleOutcome;
use mailmate_common::rules::rule::RuleKind;
use mailmate_common::shadow::ShadowOutcomeRow;

/// Aggregate a rule's performance from the filing-feedback rows that name it
/// (`matched_rule_id`) and its recorded shadow firings.
///
/// Live precision comes from the feedback polarity (the user agreed vs. overrode); shadow
/// precision comes from `matched_later_user_action`.
#[must_use]
pub fn aggregate_rule_outcome(
    rule_id: &RuleId,
    rule_kind: RuleKind,
    filing_rows: &[FilingFeedbackRow],
    shadow_rows: &[ShadowOutcomeRow],
) -> RuleOutcome {
    let mut outcome = RuleOutcome::new(rule_id.clone(), rule_kind);
    for row in filing_rows {
        if row.matched_rule_id.as_ref() != Some(rule_id) {
            continue;
        }
        outcome.fire_count += 1;
        match row.polarity {
            FeedbackPolarity::Positive => outcome.positive_count += 1,
            FeedbackPolarity::Negative => outcome.negative_count += 1,
        }
    }
    for row in shadow_rows {
        if &row.rule_id != rule_id {
            continue;
        }
        outcome.shadow_total += 1;
        if row.matched_later_user_action == Some(true) {
            outcome.shadow_matched += 1;
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::feedback::{FilingFeedback, PinnedVersions};
    use mailmate_common::ids::{FolderId, MessageId, RuleVersionId};
    use mailmate_common::rules::effect::RuleEffect;
    use mailmate_common::time::Timestamp;

    fn filing(rule: Option<&str>, polarity: FeedbackPolarity) -> FilingFeedbackRow {
        FilingFeedbackRow {
            id: FilingFeedback::fresh_id(),
            message_id: MessageId::fresh(),
            pinned_versions: PinnedVersions::default(),
            sender_domain: Some("stripe.com".to_owned()),
            ai_suggested_folder: None,
            human_chosen_folder: FolderId::from("Receipts"),
            basis: None,
            matched_rule_id: rule.map(RuleId::from),
            polarity,
            created_at: Timestamp::now(),
        }
    }

    fn shadow(rule: &str, matched: Option<bool>) -> ShadowOutcomeRow {
        let mut row = ShadowOutcomeRow::new(
            RuleKind::Action,
            RuleId::from(rule),
            RuleVersionId::from("rv_1"),
            MessageId::fresh(),
            RuleEffect::new(),
            "requires_review",
        );
        row.matched_later_user_action = matched;
        row
    }

    #[test]
    fn aggregates_live_and_shadow_signal_for_the_named_rule_only() {
        let rule = RuleId::from("rule_target");
        let filing_rows = vec![
            filing(Some("rule_target"), FeedbackPolarity::Positive),
            filing(Some("rule_target"), FeedbackPolarity::Positive),
            filing(Some("rule_target"), FeedbackPolarity::Negative),
            // A different rule, and an un-attributed move — neither counts.
            filing(Some("rule_other"), FeedbackPolarity::Positive),
            filing(None, FeedbackPolarity::Positive),
        ];
        let shadow_rows = vec![
            shadow("rule_target", Some(true)),
            shadow("rule_target", Some(false)),
            shadow("rule_target", None),
            shadow("rule_other", Some(true)),
        ];
        let outcome = aggregate_rule_outcome(&rule, RuleKind::Action, &filing_rows, &shadow_rows);
        assert_eq!(outcome.fire_count, 3);
        assert_eq!(outcome.positive_count, 2);
        assert_eq!(outcome.negative_count, 1);
        assert_eq!(outcome.precision(), Some(2.0 / 3.0));
        assert_eq!(outcome.shadow_total, 3, "only this rule's shadow firings");
        assert_eq!(outcome.shadow_matched, 1);
        assert_eq!(outcome.shadow_precision(), Some(1.0 / 3.0));
    }

    #[test]
    fn a_rule_with_no_signal_yields_an_empty_outcome() {
        let outcome = aggregate_rule_outcome(
            &RuleId::from("rule_quiet"),
            RuleKind::Classification,
            &[],
            &[],
        );
        assert_eq!(outcome.fire_count, 0);
        assert_eq!(outcome.precision(), None);
        assert_eq!(outcome.shadow_precision(), None);
    }
}
