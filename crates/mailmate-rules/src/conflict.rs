//! Conflict detection over the JSON-AST condition language.
//!
//! Two layers, both sound (a reported conflict is real; an unreported one may be missed for
//! opaque conditions — we never *invent* a conflict):
//!
//! 1. **Exact contradiction** — a candidate and an existing rule of the same kind and scope whose
//!    conditions are **structurally equivalent** (order-insensitive across `all`/`any`) and whose
//!    effects contradict (a move to two different folders, or opposite junk states) →
//!    [`ContradictoryEffect`](ConflictKind::ContradictoryEffect).
//! 2. **Subsumption / co-match overlap (Phase 7)** — beyond exact equivalence: when the candidate
//!    and an existing rule are both **equality-predicate conjunctions** (a bare `field == value`,
//!    or an `all` of them — the shape induction emits), we can reason about their matched sets
//!    exactly. They **co-match** some mail unless a shared field forces different values (disjoint).
//!    If they co-match and impose *different* effects, that is an [`Overlap`](ConflictKind::Overlap)
//!    conflict the human must adjudicate — classified as *subsumption* (one predicate-set is a
//!    superset of the other → its matched set is a subset) or *partial overlap*. Severity is High
//!    when the effects strictly contradict (move/junk), else Medium.
//!
//! Conditions that aren't pure equality conjunctions (`any`, `not`, regex/`matches`, ranges) are
//! **opaque** to the overlap layer and skipped there — we would rather miss a subtle overlap than
//! report a false one. `UnsafeEscalation` remains reserved.

use std::collections::BTreeMap;

use mailmate_common::rules::condition::{Condition, FieldValue, Operator};
use mailmate_common::rules::evaluation::{ConflictKind, ConflictSeverity, RuleConflict};
use mailmate_common::rules::rule::{EvaluatableRule, RuleDraft};

/// Detect conflicts between `candidate` and each `existing` rule.
#[must_use]
pub fn detect_conflicts(candidate: &RuleDraft, existing: &[EvaluatableRule]) -> Vec<RuleConflict> {
    let mut conflicts = Vec::new();
    for rule in existing {
        if rule.kind != candidate.kind || rule.scope != candidate.scope {
            continue;
        }
        if conditions_equivalent(&candidate.condition, &rule.version.condition) {
            // Exact-equivalent conditions fire on EXACTLY the same mail, so ANY difference in effect
            // is a total conflict — not just a move/junk contradiction. `RuleEffect::contradicts`
            // only catches move/junk; a classification pair that sets a *different label or priority*
            // on the identical condition (e.g. `list_id==news → newsletter` vs `… → promotions`)
            // disagrees on every message yet would slip through if we only checked `contradicts`. So
            // report whenever the effects differ at all: High when they strictly contradict, else
            // Medium. An identical effect is a harmless duplicate (no conflict).
            if candidate.effect != rule.version.effect {
                let severity = if candidate.effect.contradicts(&rule.version.effect) {
                    ConflictSeverity::High
                } else {
                    ConflictSeverity::Medium
                };
                conflicts.push(RuleConflict {
                    existing_rule_id: Some(rule.rule_id.clone()),
                    kind: ConflictKind::ContradictoryEffect,
                    severity,
                    description: format!(
                        "candidate shares rule {}'s condition but imposes a different effect",
                        rule.rule_id
                    ),
                });
            }
            // The overlap layer below would only re-derive this same pair — done with it.
            continue;
        }
        // Beyond exact equivalence: subsumption / co-match overlap with a differing effect.
        if let Some(conflict) = overlap_conflict(candidate, rule) {
            conflicts.push(conflict);
        }
    }
    conflicts
}

/// A [`ConflictKind::Overlap`] between the candidate and `rule` when both are equality-predicate
/// conjunctions that co-match some mail and impose *different* effects — else `None`.
fn overlap_conflict(candidate: &RuleDraft, rule: &EvaluatableRule) -> Option<RuleConflict> {
    let cand = equality_conjunction(&candidate.condition)?;
    let exist = equality_conjunction(&rule.version.condition)?;

    // Disjoint? If any field both constrain takes different values, no message satisfies both —
    // they never co-match, so there is no conflict.
    for (field, value) in &cand {
        if let Some(existing_value) = exist.get(field) {
            if existing_value != value {
                return None;
            }
        }
    }

    // They co-match. A shared effect is redundant but harmless (not a conflict); only a *different*
    // effect on the same mail needs human adjudication.
    if candidate.effect == rule.version.effect {
        return None;
    }

    let (verb, severity) = if candidate.effect.contradicts(&rule.version.effect) {
        ("contradicts", ConflictSeverity::High)
    } else {
        ("differs from", ConflictSeverity::Medium)
    };
    // Classify the matched-set relationship by predicate-set containment (more predicates ⇒ a
    // strictly smaller matched set). Equal sets were handled by the equivalence branch above.
    let cand_fields: std::collections::BTreeSet<&String> = cand.keys().collect();
    let exist_fields: std::collections::BTreeSet<&String> = exist.keys().collect();
    let relation = if cand_fields.is_superset(&exist_fields) {
        "is more specific than (subsumed by)"
    } else if cand_fields.is_subset(&exist_fields) {
        "is more general than (subsumes)"
    } else {
        "partially overlaps"
    };
    Some(RuleConflict {
        existing_rule_id: Some(rule.rule_id.clone()),
        kind: ConflictKind::Overlap,
        severity,
        description: format!(
            "candidate {relation} rule {} and its effect {verb} that rule's on the mail they both match",
            rule.rule_id
        ),
    })
}

/// Represent a condition as the `field → value` map of an **equality-predicate conjunction** — a
/// bare `field == value`, or an `all` of them. Returns `None` (opaque) for anything else (`any`,
/// `not`, a non-`Eq` operator, or a field constrained to two different values, which is
/// unsatisfiable and not worth reasoning about), so the overlap layer simply skips it.
fn equality_conjunction(condition: &Condition) -> Option<BTreeMap<String, FieldValue>> {
    let mut map = BTreeMap::new();
    collect_equalities(condition, &mut map).then_some(map)
}

fn collect_equalities(condition: &Condition, map: &mut BTreeMap<String, FieldValue>) -> bool {
    match condition {
        Condition::Predicate(p) if p.op == Operator::Eq => {
            // A field bound twice (to whatever value) makes the conjunction's matched set
            // intractable to compare cleanly — bail to opaque rather than guess.
            map.insert(p.field.clone(), p.value.clone()).is_none()
        }
        Condition::All { all } => all.iter().all(|child| collect_equalities(child, map)),
        // Any/Not/non-Eq predicate: opaque to exact matched-set reasoning.
        _ => false,
    }
}

/// Whether two conditions are structurally equivalent, ignoring child order under
/// `all`/`any` (so `all[A, B]` equals `all[B, A]`).
#[must_use]
pub fn conditions_equivalent(a: &Condition, b: &Condition) -> bool {
    canonical(a) == canonical(b)
}

/// Canonicalize a condition to a JSON value with `all`/`any` child arrays sorted, so
/// structural equality is order-insensitive.
fn canonical(condition: &Condition) -> serde_json::Value {
    use serde_json::{json, Value};
    match condition {
        Condition::All { all } => json!({ "all": sorted_children(all) }),
        Condition::Any { any } => json!({ "any": sorted_children(any) }),
        Condition::Not { not } => json!({ "not": canonical(not) }),
        Condition::Predicate(predicate) => serde_json::to_value(predicate).unwrap_or(Value::Null),
    }
}

fn sorted_children(children: &[Condition]) -> Vec<serde_json::Value> {
    let mut values: Vec<serde_json::Value> = children.iter().map(canonical).collect();
    values.sort_by_key(serde_json::Value::to_string);
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::rules::condition::{FieldValue, Operator, Predicate};
    use mailmate_common::rules::effect::RuleEffect;
    use mailmate_common::rules::rule::{
        HierarchyBand, RiskLevel, RuleKind, RuleScope, RuleStatus, RuleVersion,
    };
    use mailmate_common::ids::{RuleId, RuleVersionId};

    fn pred(field: &str, value: &str) -> Condition {
        Condition::Predicate(Predicate {
            field: field.to_owned(),
            op: Operator::Eq,
            value: FieldValue::Text(value.to_owned()),
        })
    }

    fn label_effect(label: &str) -> RuleEffect {
        RuleEffect {
            set_labels: vec![label.to_owned()],
            ..RuleEffect::new()
        }
    }

    fn candidate(condition: Condition, effect: RuleEffect) -> RuleDraft {
        RuleDraft {
            kind: RuleKind::Classification,
            scope: RuleScope::Global,
            condition,
            effect,
        }
    }

    fn existing(id: &str, condition: Condition, effect: RuleEffect) -> EvaluatableRule {
        EvaluatableRule {
            rule_id: RuleId::from(id),
            kind: RuleKind::Classification,
            scope: RuleScope::Global,
            band: HierarchyBand::LearnedActive,
            status: RuleStatus::Active,
            version: RuleVersion {
                id: RuleVersionId::from("rv_1"),
                version_number: 1,
                condition,
                effect,
                risk_level: RiskLevel::Low,
            },
        }
    }

    #[test]
    fn equivalence_is_order_insensitive_under_all() {
        let a = Condition::All {
            all: vec![pred("x", "1"), pred("y", "2")],
        };
        let b = Condition::All {
            all: vec![pred("y", "2"), pred("x", "1")],
        };
        assert!(conditions_equivalent(&a, &b));

        let c = Condition::All {
            all: vec![pred("x", "1"), pred("y", "3")],
        };
        assert!(!conditions_equivalent(&a, &c));
    }

    #[test]
    fn a_more_specific_candidate_subsumed_by_an_existing_rule_with_a_different_effect_conflicts() {
        // Existing: auth_result == fail → suspicious. Candidate: auth_result == fail AND
        // no_prior_contact == true → newsletter. Every message the candidate matches ALSO matches
        // the existing rule, which would label it `suspicious` — a real overlap the user must judge.
        let existing_rule = existing("rule_suspicious", pred("auth_result", "fail"), label_effect("suspicious"));
        let cand = candidate(
            Condition::All {
                all: vec![pred("auth_result", "fail"), pred("no_prior_contact", "true")],
            },
            label_effect("newsletter"),
        );
        let conflicts = detect_conflicts(&cand, std::slice::from_ref(&existing_rule));
        assert_eq!(conflicts.len(), 1, "the subsumption is reported");
        assert_eq!(conflicts[0].kind, ConflictKind::Overlap);
        assert_eq!(conflicts[0].existing_rule_id.as_ref().unwrap().as_str(), "rule_suspicious");
        assert!(conflicts[0].description.contains("subsumed by"), "{}", conflicts[0].description);
    }

    #[test]
    fn an_identical_condition_with_a_different_label_conflicts_even_without_a_move_or_junk_clash() {
        // The gap RuleEffect::contradicts misses: two classification rules with the SAME condition
        // but DIFFERENT labels fire on exactly the same mail and disagree on every message — a real
        // conflict, though neither moves nor junks. It must be reported (else it auto-shadows and
        // silently fights the existing rule).
        let existing_rule = existing("rule_news", pred("list_id", "news"), label_effect("newsletter"));
        let cand = candidate(pred("list_id", "news"), label_effect("promotions"));
        let conflicts = detect_conflicts(&cand, std::slice::from_ref(&existing_rule));
        assert_eq!(conflicts.len(), 1, "same condition, different label is a conflict");
        assert_eq!(conflicts[0].kind, ConflictKind::ContradictoryEffect);
        assert!(conflicts[0].description.contains("different effect"), "{}", conflicts[0].description);
    }

    #[test]
    fn an_identical_condition_with_an_identical_effect_is_a_harmless_duplicate() {
        // Same condition AND same effect → a redundant duplicate, not a conflict.
        let existing_rule = existing("rule_news", pred("list_id", "news"), label_effect("newsletter"));
        let cand = candidate(pred("list_id", "news"), label_effect("newsletter"));
        assert!(
            detect_conflicts(&cand, std::slice::from_ref(&existing_rule)).is_empty(),
            "an exact duplicate is redundant, not conflicting"
        );
    }

    #[test]
    fn disjoint_rules_on_a_shared_field_do_not_conflict() {
        // auth_result == fail vs auth_result == pass: no message satisfies both, so no overlap.
        let existing_rule = existing("rule_pass", pred("auth_result", "pass"), label_effect("trusted"));
        let cand = candidate(pred("auth_result", "fail"), label_effect("suspicious"));
        assert!(
            detect_conflicts(&cand, std::slice::from_ref(&existing_rule)).is_empty(),
            "different values on the shared field are disjoint, not conflicting"
        );
    }

    #[test]
    fn co_matching_rules_with_the_same_effect_are_not_a_conflict() {
        // Candidate is more specific but agrees on the effect → redundant, harmless, not reported.
        let existing_rule = existing("rule_news", pred("list_id", "news"), label_effect("newsletter"));
        let cand = candidate(
            Condition::All {
                all: vec![pred("list_id", "news"), pred("sender_domain", "x.com")],
            },
            label_effect("newsletter"),
        );
        assert!(
            detect_conflicts(&cand, std::slice::from_ref(&existing_rule)).is_empty(),
            "same effect on overlapping mail is redundancy, not a conflict"
        );
    }

    #[test]
    fn a_partial_overlap_with_a_contradicting_move_is_high_severity() {
        // Two action rules sharing sender_domain but each adding a different field, with
        // contradicting move targets → they co-match some mail and would move it to two folders.
        let existing_rule = EvaluatableRule {
            kind: RuleKind::Action,
            ..existing(
                "rule_move_a",
                Condition::All {
                    all: vec![pred("sender_domain", "acme.com"), pred("has_invoice", "true")],
                },
                RuleEffect { move_to: Some("Receipts".to_owned()), ..RuleEffect::new() },
            )
        };
        let cand = RuleDraft {
            kind: RuleKind::Action,
            ..candidate(
                Condition::All {
                    all: vec![pred("sender_domain", "acme.com"), pred("urgent", "true")],
                },
                RuleEffect { move_to: Some("Urgent".to_owned()), ..RuleEffect::new() },
            )
        };
        let conflicts = detect_conflicts(&cand, std::slice::from_ref(&existing_rule));
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].kind, ConflictKind::Overlap);
        assert_eq!(conflicts[0].severity, ConflictSeverity::High, "contradicting moves are high");
        assert!(conflicts[0].description.contains("partially overlaps"), "{}", conflicts[0].description);
    }

    #[test]
    fn an_opaque_any_condition_is_skipped_not_falsely_conflicted() {
        // An `any` condition is not an equality conjunction → the overlap layer cannot reason about
        // its matched set soundly, so it reports nothing rather than a guess.
        let existing_rule = existing(
            "rule_any",
            Condition::Any { any: vec![pred("a", "1"), pred("b", "2")] },
            label_effect("x"),
        );
        let cand = candidate(pred("a", "1"), label_effect("y"));
        assert!(
            detect_conflicts(&cand, std::slice::from_ref(&existing_rule)).is_empty(),
            "opaque conditions are skipped, never falsely conflicted"
        );
    }
}

