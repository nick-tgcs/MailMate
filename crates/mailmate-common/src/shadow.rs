//! `shadow_outcomes` — the sole owner of shadow-rule performance.
//!
//! A rule promoted to `shadow_mode` is evaluated but never applied; each time it *would*
//! have fired during live evaluation, the runtime records one of these rows (what it would
//! have proposed, the policy outcome it would have hit, and — once known — whether the user
//! later did the same thing manually). `RuleOutcome` derives shadow precision from these,
//! so the table is queried, never duplicated. (The crystallization *back-test* over history
//! is a separate, in-memory gate; this table is the live-shadow record.)

use serde::{Deserialize, Serialize};

use crate::ids::{MessageId, RuleId, RuleVersionId, ShadowOutcomeId};
use crate::rules::effect::RuleEffect;
use crate::rules::rule::RuleKind;
use crate::time::Timestamp;

/// One recorded shadow firing.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ShadowOutcomeRow {
    /// The row id (`shad_…`).
    pub id: ShadowOutcomeId,
    /// The kind of the shadow rule (disambiguates `rule_id`).
    pub rule_kind: RuleKind,
    /// The shadow rule that fired.
    pub rule_id: RuleId,
    /// The exact version that fired.
    pub rule_version_id: RuleVersionId,
    /// The message that triggered it (a shadow *rule* is always message-triggered).
    pub message_id: MessageId,
    /// The effect it would have proposed.
    pub would_have_action: RuleEffect,
    /// The `PolicyOutcome` label it would have hit (`allowed`/`requires_review`/`blocked`).
    pub would_have_policy_outcome: String,
    /// Whether the user later took the same action manually (`None` until known).
    pub matched_later_user_action: Option<bool>,
    /// When recorded.
    pub created_at: Timestamp,
}

impl ShadowOutcomeRow {
    /// A fresh shadow-outcome row stamped now.
    #[must_use]
    pub fn new(
        rule_kind: RuleKind,
        rule_id: RuleId,
        rule_version_id: RuleVersionId,
        message_id: MessageId,
        would_have_action: RuleEffect,
        would_have_policy_outcome: impl Into<String>,
    ) -> Self {
        Self {
            id: ShadowOutcomeId::fresh(),
            rule_kind,
            rule_id,
            rule_version_id,
            message_id,
            would_have_action,
            would_have_policy_outcome: would_have_policy_outcome.into(),
            matched_later_user_action: None,
            created_at: Timestamp::now(),
        }
    }

    /// Record whether the user later matched this shadow action.
    #[must_use]
    pub fn with_match(mut self, matched: bool) -> Self {
        self.matched_later_user_action = Some(matched);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_stamps_id_and_defaults_match_to_unknown() {
        let row = ShadowOutcomeRow::new(
            RuleKind::Action,
            RuleId::from("rule_1"),
            RuleVersionId::from("rv_1"),
            MessageId::from("msg_1"),
            RuleEffect {
                move_to: Some("Receipts".to_owned()),
                ..RuleEffect::new()
            },
            "requires_review",
        );
        assert!(row.id.as_str().starts_with("shad_"));
        assert_eq!(row.matched_later_user_action, None);
        assert_eq!(row.with_match(true).matched_later_user_action, Some(true));
    }
}
