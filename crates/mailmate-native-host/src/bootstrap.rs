//! Cold-start bootstrap: the disabled **starter rules** a fresh install ships (§3.5), so the
//! product is useful in session one — there is something concrete to review and one-tap activate
//! before any correction has been made.
//!
//! These are deterministic heuristic priors, expressed in the same JSON-AST condition language as
//! every other rule and keyed only on features the [`DeterministicFeatureExtractor`] actually
//! emits (so they resolve against the planner's field environment). They are imported as fresh
//! **drafts** through the ordinary [`ImportExportService`] path — never auto-active — at the
//! lowest authority band, so any learned rule outranks them. The import is idempotent across
//! launches (a name collision is skipped), so re-running on every start adds nothing once seeded.
//!
//! [`DeterministicFeatureExtractor`]: mailmate_ml
//! [`ImportExportService`]: mailmate_core::ImportExportService

use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::manifest::{ExportedRule, RuleManifest};
use mailmate_common::rules::rule::{HierarchyBand, RiskLevel, RuleKind, RuleScope, RuleStatus};

/// The name prefix every starter rule is imported under (`starter-<source-id>`), so a re-run
/// collides on the name and is skipped rather than duplicated.
pub const STARTER_PREFIX: &str = "starter";

/// One `field <op> value` leaf.
fn predicate(field: &str, op: Operator, value: FieldValue) -> Condition {
    Condition::Predicate(Predicate {
        field: field.to_owned(),
        op,
        value,
    })
}

/// A starter rule in identity-free manifest form. `band`/`status` round-trip but an import always
/// lands a non-firing draft regardless.
fn starter(
    source_rule_id: &str,
    kind: RuleKind,
    condition: Condition,
    effect: RuleEffect,
    risk_level: RiskLevel,
) -> ExportedRule {
    ExportedRule {
        source_rule_id: source_rule_id.to_owned(),
        kind,
        scope: RuleScope::Global,
        // The lowest authority: a heuristic prior any learned rule (or a human-hard rule) outranks.
        band: HierarchyBand::DefaultFallback,
        // Informational only — the import forces `Draft`. We state the intent honestly.
        status: RuleStatus::Draft,
        condition,
        effect,
        risk_level,
    }
}

/// The §3.5 cold-start starter rules: a fresh install's two one-tap heuristics.
///
/// 1. **Newsletters** — a bulk message carrying a `List-Unsubscribe` is almost always a
///    newsletter (`has_unsubscribe ∧ is_bulk → label newsletters`). Low risk.
/// 2. **Unauthenticated stranger** — mail that fails DMARC *and* is from a sender you have never
///    heard from is worth a second look (`¬dmarc_pass ∧ sender_seen_count < 1 → label
///    suspicious`). Inform-only label (never an auto-junk); medium risk.
///
/// Both key only on features the extractor emits, so they resolve against the live field
/// environment (proven by the per-predicate test below).
#[must_use]
pub fn starter_rules_manifest() -> RuleManifest {
    RuleManifest::new(vec![
        starter(
            "newsletters",
            RuleKind::Classification,
            Condition::All {
                all: vec![
                    predicate("has_unsubscribe", Operator::Eq, FieldValue::Bool(true)),
                    predicate("is_bulk", Operator::Eq, FieldValue::Bool(true)),
                ],
            },
            RuleEffect {
                set_labels: vec!["newsletters".to_owned()],
                ..RuleEffect::new()
            },
            RiskLevel::Low,
        ),
        starter(
            "suspicious-unauthenticated",
            RuleKind::Classification,
            Condition::All {
                all: vec![
                    predicate("dmarc_pass", Operator::Eq, FieldValue::Bool(false)),
                    predicate("sender_seen_count", Operator::Lt, FieldValue::Float(1.0)),
                ],
            },
            RuleEffect {
                set_labels: vec!["suspicious".to_owned()],
                ..RuleEffect::new()
            },
            RiskLevel::Medium,
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use mailmate_rules::evaluator::evaluate_condition;

    /// Build a field environment the way the planner's `base_fields` does for the relevant keys.
    fn env(pairs: &[(&str, FieldValue)]) -> BTreeMap<String, FieldValue> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    #[test]
    fn the_two_starter_rules_are_shipped_as_drafts() {
        let manifest = starter_rules_manifest();
        assert_eq!(manifest.len(), 2);
        assert!(manifest
            .rules
            .iter()
            .all(|r| r.status == RuleStatus::Draft && r.band == HierarchyBand::DefaultFallback));
        assert_eq!(manifest.rules[0].source_rule_id, "newsletters");
        assert_eq!(
            manifest.rules[1].source_rule_id,
            "suspicious-unauthenticated"
        );
    }

    #[test]
    fn each_starter_predicate_resolves_against_the_extractors_field_keys() {
        // This is the §3.5 guard the synthesis demanded: prove every starter predicate keys on a
        // field the extractor actually emits, with an operator the evaluator interprets — a dead
        // key would silently never fire.
        let manifest = starter_rules_manifest();
        let newsletters = &manifest.rules[0].condition;
        let suspicious = &manifest.rules[1].condition;

        // A bulk message with a List-Unsubscribe header → newsletters fires.
        let bulk_list = env(&[
            ("has_unsubscribe", FieldValue::Bool(true)),
            ("is_bulk", FieldValue::Bool(true)),
        ]);
        assert!(evaluate_condition(newsletters, &bulk_list));
        // A bulk message WITHOUT unsubscribe does not.
        let bulk_only = env(&[
            ("has_unsubscribe", FieldValue::Bool(false)),
            ("is_bulk", FieldValue::Bool(true)),
        ]);
        assert!(!evaluate_condition(newsletters, &bulk_only));

        // DMARC-failing mail from a never-seen sender → suspicious fires.
        let unauth_stranger = env(&[
            ("dmarc_pass", FieldValue::Bool(false)),
            ("sender_seen_count", FieldValue::Float(0.0)),
        ]);
        assert!(evaluate_condition(suspicious, &unauth_stranger));
        // The same DMARC failure from a known correspondent does not.
        let unauth_known = env(&[
            ("dmarc_pass", FieldValue::Bool(false)),
            ("sender_seen_count", FieldValue::Float(42.0)),
        ]);
        assert!(!evaluate_condition(suspicious, &unauth_known));
        // A passing, known sender does not.
        let clean = env(&[
            ("dmarc_pass", FieldValue::Bool(true)),
            ("sender_seen_count", FieldValue::Float(5.0)),
        ]);
        assert!(!evaluate_condition(suspicious, &clean));
    }
}
