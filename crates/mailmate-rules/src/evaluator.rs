//! The deterministic condition evaluator: interpret a [`Condition`] tree against a
//! resolved field environment. Pure and total — a missing field or a type mismatch yields
//! `false` (fail-closed), never a panic.

use std::collections::BTreeMap;

use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
use mailmate_common::time::Timestamp;
use regex::Regex;

/// Evaluate a condition tree against the `field name → value` environment.
#[must_use]
pub fn evaluate_condition(condition: &Condition, env: &BTreeMap<String, FieldValue>) -> bool {
    match condition {
        Condition::All { all } => all.iter().all(|c| evaluate_condition(c, env)),
        Condition::Any { any } => any.iter().any(|c| evaluate_condition(c, env)),
        Condition::Not { not } => !evaluate_condition(not, env),
        Condition::Predicate(predicate) => evaluate_predicate(predicate, env),
    }
}

fn evaluate_predicate(predicate: &Predicate, env: &BTreeMap<String, FieldValue>) -> bool {
    let field = env.get(&predicate.field);

    // `exists` is the only operator that is meaningful for an absent field.
    if predicate.op == Operator::Exists {
        return field.is_some_and(FieldValue::is_present);
    }

    match field {
        Some(value) if value.is_present() => apply_op(predicate.op, value, &predicate.value),
        _ => false,
    }
}

fn apply_op(op: Operator, field: &FieldValue, value: &FieldValue) -> bool {
    match op {
        Operator::Eq => values_equal(field, value),
        Operator::In => scalar_in_set(field, value),
        Operator::Contains => contains(field, value),
        Operator::ContainsAny => contains_set(field, value, SetMode::Any),
        Operator::ContainsAll => contains_set(field, value, SetMode::All),
        Operator::Gt => numeric_cmp(field, value, |a, b| a > b),
        Operator::Gte => numeric_cmp(field, value, |a, b| a >= b),
        Operator::Lt => numeric_cmp(field, value, |a, b| a < b),
        Operator::Lte => numeric_cmp(field, value, |a, b| a <= b),
        Operator::Before => datetime_cmp(field, value, |a, b| a < b),
        Operator::After => datetime_cmp(field, value, |a, b| a > b),
        Operator::MatchesRegex => matches_regex(field, value),
        // `exists` handled before reaching here.
        Operator::Exists => field.is_present(),
    }
}

/// Equality with numeric coercion (so `Int(5)` equals `Float(5.0)`).
fn values_equal(field: &FieldValue, value: &FieldValue) -> bool {
    match (field.as_number(), value.as_number()) {
        (Some(a), Some(b)) => (a - b).abs() < f64::EPSILON,
        _ => field == value,
    }
}

/// `field` (a scalar string) is a member of `value` (a set).
fn scalar_in_set(field: &FieldValue, value: &FieldValue) -> bool {
    match (field.as_text(), value.as_set()) {
        (Some(needle), Some(set)) => set.contains(&needle),
        _ => false,
    }
}

/// `field` contains `value`: substring for a text field, membership for a set field.
fn contains(field: &FieldValue, value: &FieldValue) -> bool {
    let Some(needle) = value.as_text() else {
        return false;
    };
    match field {
        FieldValue::TextSet(set) => set.iter().any(|s| s == needle),
        FieldValue::Text(haystack) => haystack.contains(needle),
        _ => false,
    }
}

enum SetMode {
    Any,
    All,
}

/// `field` contains any/all of `value` (a set of needles).
fn contains_set(field: &FieldValue, value: &FieldValue, mode: SetMode) -> bool {
    let Some(needles) = value.as_set() else {
        return false;
    };
    let test = |needle: &str| match field {
        FieldValue::TextSet(set) => set.iter().any(|s| s == needle),
        FieldValue::Text(haystack) => haystack.contains(needle),
        _ => false,
    };
    match mode {
        SetMode::Any => needles.iter().any(|n| test(n)),
        SetMode::All => needles.iter().all(|n| test(n)),
    }
}

fn numeric_cmp(field: &FieldValue, value: &FieldValue, cmp: impl Fn(f64, f64) -> bool) -> bool {
    match (field.as_number(), value.as_number()) {
        (Some(a), Some(b)) => cmp(a, b),
        _ => false,
    }
}

fn datetime_cmp(
    field: &FieldValue,
    value: &FieldValue,
    cmp: impl Fn(Timestamp, Timestamp) -> bool,
) -> bool {
    match (
        field
            .as_text()
            .and_then(|s| Timestamp::parse_rfc3339(s).ok()),
        value
            .as_text()
            .and_then(|s| Timestamp::parse_rfc3339(s).ok()),
    ) {
        (Some(a), Some(b)) => cmp(a, b),
        _ => false,
    }
}

/// Regex match. An invalid pattern fails closed (`false`) — the rule validator rejects bad
/// patterns at creation, and at evaluation time we never want a malformed pattern to match.
fn matches_regex(field: &FieldValue, value: &FieldValue) -> bool {
    match (field.as_text(), value.as_text()) {
        (Some(text), Some(pattern)) => Regex::new(pattern).is_ok_and(|re| re.is_match(text)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::rules::condition::Operator;

    fn env(pairs: &[(&str, FieldValue)]) -> BTreeMap<String, FieldValue> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    fn predicate(field: &str, op: Operator, value: FieldValue) -> Condition {
        Condition::Predicate(Predicate {
            field: field.to_owned(),
            op,
            value,
        })
    }

    #[test]
    fn eq_in_and_contains_operators() {
        let e = env(&[
            ("sender_domain", FieldValue::Text("github.com".to_owned())),
            (
                "labels",
                FieldValue::TextSet(vec!["financial".to_owned(), "receipt".to_owned()]),
            ),
            (
                "subject",
                FieldValue::Text("your receipt is ready".to_owned()),
            ),
        ]);

        assert!(evaluate_condition(
            &predicate(
                "sender_domain",
                Operator::In,
                FieldValue::TextSet(vec!["github.com".to_owned(), "stripe.com".to_owned()])
            ),
            &e
        ));
        assert!(evaluate_condition(
            &predicate(
                "labels",
                Operator::Contains,
                FieldValue::Text("financial".to_owned())
            ),
            &e
        ));
        assert!(evaluate_condition(
            &predicate(
                "subject",
                Operator::ContainsAny,
                FieldValue::TextSet(vec!["receipt".to_owned(), "invoice".to_owned()])
            ),
            &e
        ));
        assert!(!evaluate_condition(
            &predicate(
                "subject",
                Operator::ContainsAll,
                FieldValue::TextSet(vec!["receipt".to_owned(), "invoice".to_owned()])
            ),
            &e
        ));
    }

    #[test]
    fn numeric_datetime_regex_and_exists() {
        let e = env(&[
            ("seen_count", FieldValue::Int(9)),
            (
                "received_at",
                FieldValue::Text("2026-06-14T10:00:00Z".to_owned()),
            ),
            ("subject", FieldValue::Text("INV-2026-0042".to_owned())),
            ("thread_id", FieldValue::Text("thread_x".to_owned())),
        ]);

        assert!(evaluate_condition(
            &predicate("seen_count", Operator::Gte, FieldValue::Int(5)),
            &e
        ));
        assert!(!evaluate_condition(
            &predicate("seen_count", Operator::Lt, FieldValue::Int(5)),
            &e
        ));
        assert!(evaluate_condition(
            &predicate(
                "received_at",
                Operator::After,
                FieldValue::Text("2026-06-01T00:00:00Z".to_owned())
            ),
            &e
        ));
        assert!(evaluate_condition(
            &predicate(
                "subject",
                Operator::MatchesRegex,
                FieldValue::Text(r"^INV-\d{4}".to_owned())
            ),
            &e
        ));
        assert!(evaluate_condition(
            &predicate("thread_id", Operator::Exists, FieldValue::Null),
            &e
        ));
        assert!(!evaluate_condition(
            &predicate("missing", Operator::Exists, FieldValue::Null),
            &e
        ));
    }

    #[test]
    fn missing_field_fails_closed_and_combinators_compose() {
        let e = env(&[("a", FieldValue::Bool(true))]);
        // Missing field → false for any non-exists op.
        assert!(!evaluate_condition(
            &predicate("missing", Operator::Eq, FieldValue::Bool(true)),
            &e
        ));
        // all/any/not compose.
        let cond = Condition::All {
            all: vec![
                predicate("a", Operator::Eq, FieldValue::Bool(true)),
                Condition::Not {
                    not: Box::new(predicate("a", Operator::Eq, FieldValue::Bool(false))),
                },
            ],
        };
        assert!(evaluate_condition(&cond, &e));
    }

    #[test]
    fn invalid_regex_fails_closed() {
        let e = env(&[("s", FieldValue::Text("abc".to_owned()))]);
        assert!(!evaluate_condition(
            &predicate(
                "s",
                Operator::MatchesRegex,
                FieldValue::Text("(".to_owned())
            ),
            &e
        ));
    }
}
