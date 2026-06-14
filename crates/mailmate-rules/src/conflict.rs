//! Conflict detection over the JSON-AST condition language.
//!
//! Phase-4 scope: `contradictory_effect` between a candidate and an existing rule of the
//! same kind and scope whose conditions are **structurally equivalent** (order-insensitive
//! across `all`/`any`) and whose effects contradict (a move to two different folders, or
//! opposite junk states). Regex-bearing conditions are opaque to overlap reasoning but
//! still participate here, because structural equivalence compares the regex pattern as
//! text — i.e. exact-duplicate detection only, exactly as the spec requires. Broader
//! overlap reasoning (`Overlap`, `UnsafeEscalation`) is reserved for a later enhancement.

use mailmate_common::rules::condition::Condition;
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
        if !conditions_equivalent(&candidate.condition, &rule.version.condition) {
            continue;
        }
        if candidate.effect.contradicts(&rule.version.effect) {
            conflicts.push(RuleConflict {
                existing_rule_id: Some(rule.rule_id.clone()),
                kind: ConflictKind::ContradictoryEffect,
                severity: ConflictSeverity::High,
                description: format!(
                    "candidate shares rule {}'s condition but contradicts its effect",
                    rule.rule_id
                ),
            });
        }
    }
    conflicts
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

    fn pred(field: &str, value: &str) -> Condition {
        Condition::Predicate(Predicate {
            field: field.to_owned(),
            op: Operator::Eq,
            value: FieldValue::Text(value.to_owned()),
        })
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
}
