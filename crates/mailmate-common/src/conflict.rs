//! The persisted form of a detected rule conflict — the `rule_conflicts` table's row.
//!
//! [`RuleConflict`](crate::rules::evaluation::RuleConflict) is the *transient* result the
//! rule engine returns for a candidate-vs-existing check. When the curator runs a conflict
//! scan over two **live** rules and finds a genuine clash, that fact is recorded here so a
//! human can resolve it: a `rule_conflicts` row names both rules, the kind, the severity,
//! and a mutable [`ConflictStatus`]. Conflicts are same-kind-only (the classification and
//! action effect spaces are disjoint), and `rule_kind` disambiguates which rule table the
//! two ids reference.

use serde::{Deserialize, Serialize};

use crate::ids::{ConflictId, RuleId};
use crate::rules::evaluation::{ConflictKind, ConflictSeverity};
use crate::rules::rule::RuleKind;
use crate::time::Timestamp;

/// A recorded conflict's resolution state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictStatus {
    /// Detected, not yet resolved.
    Open,
    /// A human reconciled the rules (edited/retired one).
    Resolved,
    /// A human judged the conflict acceptable and dismissed it.
    Ignored,
}

impl ConflictStatus {
    /// The stable snake_case label stored in `rule_conflicts.status`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
            Self::Ignored => "ignored",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "resolved" => Some(Self::Resolved),
            "ignored" => Some(Self::Ignored),
            _ => None,
        }
    }

    /// Whether this is a terminal disposition (a human has dealt with the conflict).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Resolved | Self::Ignored)
    }
}

/// One recorded conflict between two live rules of the same kind.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RuleConflictRecord {
    /// The conflict id (`conf_…`).
    pub id: ConflictId,
    /// Which rule table both ids reference (conflicts are same-kind-only).
    pub rule_kind: RuleKind,
    /// The first conflicting rule.
    pub rule_a_id: RuleId,
    /// The second conflicting rule.
    pub rule_b_id: RuleId,
    /// What kind of conflict.
    pub conflict_kind: ConflictKind,
    /// How serious it is.
    pub severity: ConflictSeverity,
    /// A human-readable explanation.
    pub description: String,
    /// The mutable resolution state.
    pub status: ConflictStatus,
    /// When detected.
    pub created_at: Timestamp,
    /// When a human resolved/ignored it, if ever.
    pub resolved_at: Option<Timestamp>,
}

impl RuleConflictRecord {
    /// A freshly-detected, `Open` conflict between two rules, stamped now.
    #[must_use]
    pub fn new(
        rule_kind: RuleKind,
        rule_a_id: RuleId,
        rule_b_id: RuleId,
        conflict_kind: ConflictKind,
        severity: ConflictSeverity,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: ConflictId::fresh(),
            rule_kind,
            rule_a_id,
            rule_b_id,
            conflict_kind,
            severity,
            description: description.into(),
            status: ConflictStatus::Open,
            created_at: Timestamp::now(),
            resolved_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_labels_round_trip_and_terminality_is_correct() {
        for status in [
            ConflictStatus::Open,
            ConflictStatus::Resolved,
            ConflictStatus::Ignored,
        ] {
            assert_eq!(ConflictStatus::from_db_str(status.as_str()), Some(status));
        }
        assert_eq!(ConflictStatus::from_db_str("nope"), None);
        assert!(!ConflictStatus::Open.is_terminal());
        assert!(ConflictStatus::Resolved.is_terminal());
        assert!(ConflictStatus::Ignored.is_terminal());
    }

    #[test]
    fn new_record_is_open_with_a_fresh_id_and_no_resolution() {
        let record = RuleConflictRecord::new(
            RuleKind::Action,
            RuleId::from("rule_a"),
            RuleId::from("rule_b"),
            ConflictKind::ContradictoryEffect,
            ConflictSeverity::High,
            "both file the same sender to different folders",
        );
        assert!(record.id.as_str().starts_with("conf_"));
        assert_eq!(record.status, ConflictStatus::Open);
        assert_eq!(record.rule_a_id, RuleId::from("rule_a"));
        assert_eq!(record.rule_b_id, RuleId::from("rule_b"));
        assert!(record.resolved_at.is_none());
    }
}
