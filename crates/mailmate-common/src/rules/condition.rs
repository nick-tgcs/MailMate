//! The rule condition language: a declarative JSON AST (`all`/`any`/`not` combinators
//! over typed `field`/`op`/`value` predicates). Not a scripting language — a constrained,
//! analyzable tree so conflict detection can reason about it, versions are diffable, the
//! UI can render it, and AI-proposed rules are safe to validate without executing code.

use serde::{Deserialize, Serialize};

/// A condition tree node.
///
/// Serializes exactly as the spec's JSON: `{ "all": [..] }`, `{ "any": [..] }`,
/// `{ "not": {..} }`, or a bare predicate `{ "field":.., "op":.., "value":.. }`. The key
/// sets are disjoint, so the untagged representation is unambiguous.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Condition {
    /// All sub-conditions must hold.
    All {
        /// The conjuncts.
        all: Vec<Condition>,
    },
    /// At least one sub-condition must hold.
    Any {
        /// The disjuncts.
        any: Vec<Condition>,
    },
    /// The sub-condition must not hold.
    Not {
        /// The negated condition.
        not: Box<Condition>,
    },
    /// A leaf predicate over a single field.
    Predicate(Predicate),
}

/// A single typed predicate: does `field` relate to `value` under `op`?
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Predicate {
    /// The field name, resolved against the evaluation environment.
    pub field: String,
    /// The comparison operator.
    pub op: Operator,
    /// The right-hand value (absent for `exists`).
    #[serde(default)]
    pub value: FieldValue,
}

/// The supported comparison operators. Symbolic spellings (`==`, `>`, `>=`, `<`, `<=`) are
/// accepted as aliases of the word forms.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operator {
    /// Equality.
    #[serde(alias = "==")]
    Eq,
    /// Scalar membership in a set value.
    In,
    /// String contains a substring, or set contains a member.
    Contains,
    /// Contains any of the given values.
    ContainsAny,
    /// Contains all of the given values.
    ContainsAll,
    /// Greater than (numeric).
    #[serde(alias = ">")]
    Gt,
    /// Greater than or equal (numeric).
    #[serde(alias = ">=")]
    Gte,
    /// Less than (numeric).
    #[serde(alias = "<")]
    Lt,
    /// Less than or equal (numeric).
    #[serde(alias = "<=")]
    Lte,
    /// Earlier than (datetime).
    Before,
    /// Later than (datetime).
    After,
    /// The field is present and non-null.
    Exists,
    /// The field matches an RE2-style regular expression (opaque to conflict overlap).
    MatchesRegex,
}

impl Operator {
    /// Whether this operator makes a predicate opaque to conflict-overlap reasoning
    /// (regex predicates participate in exact-duplicate detection only).
    #[must_use]
    pub fn is_opaque_to_overlap(self) -> bool {
        matches!(self, Self::MatchesRegex)
    }
}

/// A value in a predicate or in the evaluation environment.
///
/// `untagged` so it serializes as the bare JSON scalar/array. Holds a float, so it derives
/// `PartialEq` but not `Eq`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum FieldValue {
    /// Absent / null (the default, e.g. for an `exists` predicate).
    #[default]
    Null,
    /// Boolean.
    Bool(bool),
    /// Integer.
    Int(i64),
    /// Floating-point.
    Float(f64),
    /// String.
    Text(String),
    /// A set of strings.
    TextSet(Vec<String>),
}

impl FieldValue {
    /// View as a number (`Int`/`Float`), for the numeric comparison operators.
    #[must_use]
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Int(n) => Some(*n as f64),
            Self::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// View as a string, for the text/datetime/regex operators.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }

    /// View as a set of strings (`TextSet`, or a single `Text` as a one-element set).
    #[must_use]
    pub fn as_set(&self) -> Option<Vec<&str>> {
        match self {
            Self::TextSet(v) => Some(v.iter().map(String::as_str).collect()),
            Self::Text(s) => Some(vec![s.as_str()]),
            _ => None,
        }
    }

    /// Whether this value is present (non-null).
    #[must_use]
    pub fn is_present(&self) -> bool {
        !matches!(self, Self::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_a_nested_all_any_not_tree() {
        let value = json!({
            "all": [
                { "field": "sender_domain", "op": "in", "value": ["github.com", "stripe.com"] },
                { "any": [
                    { "field": "subject_normalized", "op": "contains_any", "value": ["receipt", "invoice"] },
                    { "not": { "field": "is_spam", "op": "eq", "value": true } }
                ] }
            ]
        });
        let cond: Condition = serde_json::from_value(value.clone()).unwrap();
        match &cond {
            Condition::All { all } => assert_eq!(all.len(), 2),
            other => panic!("expected All, got {other:?}"),
        }
        // Round-trips back to the same JSON.
        assert_eq!(serde_json::to_value(&cond).unwrap(), value);
    }

    #[test]
    fn symbolic_operator_aliases_parse() {
        let p: Predicate =
            serde_json::from_value(json!({ "field": "sender_seen_count", "op": ">=", "value": 5 }))
                .unwrap();
        assert_eq!(p.op, Operator::Gte);
        assert_eq!(p.value, FieldValue::Int(5));
    }

    #[test]
    fn exists_predicate_needs_no_value() {
        let p: Predicate =
            serde_json::from_value(json!({ "field": "thread_id", "op": "exists" })).unwrap();
        assert_eq!(p.op, Operator::Exists);
        assert_eq!(p.value, FieldValue::Null);
    }

    #[test]
    fn field_value_views_coerce() {
        assert_eq!(FieldValue::Int(3).as_number(), Some(3.0));
        assert_eq!(FieldValue::Float(2.5).as_number(), Some(2.5));
        assert_eq!(FieldValue::Text("x".to_owned()).as_text(), Some("x"));
        assert_eq!(
            FieldValue::TextSet(vec!["a".to_owned(), "b".to_owned()]).as_set(),
            Some(vec!["a", "b"])
        );
        assert!(!FieldValue::Null.is_present());
        assert!(FieldValue::Bool(false).is_present());
    }
}
