//! The policy-guard vocabulary: the canonical [`PolicyOutcome`], the message signals a
//! guard reasons over ([`PolicyContext`]), and the per-policy check record.
//!
//! [`PolicyOutcome`] is the **one** canonical vocabulary for the allowed / requires-review
//! / blocked concept everywhere — domain, storage, and the protocol. Its wire labels are
//! snake_case: `allowed` / `requires_review` / `blocked`.

use serde::{Deserialize, Serialize};

use crate::ids::MessageId;

/// The hard-safety result of evaluating one action.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PolicyOutcome {
    /// The action may be applied automatically.
    Allowed,
    /// The action may only be applied after explicit human review.
    RequiresReview {
        /// Why review is required (human-readable, redacted).
        reason: String,
    },
    /// The action is forbidden by a hard policy and must never be applied.
    Blocked {
        /// The hard policy that forbids it.
        policy_id: String,
        /// Why it is blocked (human-readable).
        reason: String,
    },
}

impl PolicyOutcome {
    /// The stable snake_case label (`allowed` / `requires_review` / `blocked`) used on the
    /// wire, in logs, and in the `PolicyOutcome` view breakdown.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::RequiresReview { .. } => "requires_review",
            Self::Blocked { .. } => "blocked",
        }
    }

    /// Whether the action may be applied without human review.
    #[must_use]
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// A category of mail that raises the bar for automatic action. The
/// `financial_security_legal_move_requires_review` policy keys off the sensitive ones.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MailCategory {
    /// Financial mail (invoices, payments, banking).
    Financial,
    /// Security mail (alerts, password resets, 2FA).
    Security,
    /// Legal mail (contracts, notices).
    Legal,
    /// Anything else — no elevated handling.
    General,
}

impl MailCategory {
    /// Whether moving this category of mail requires review unless explicitly allowed.
    #[must_use]
    pub fn is_sensitive(self) -> bool {
        matches!(self, Self::Financial | Self::Security | Self::Legal)
    }
}

/// What caused planning to run. Carried for provenance; no hard policy branches on it
/// (the follow-up "send floor" is enforced by the absence of any send action, not by a
/// trigger-specific policy).
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    /// New mail arrived.
    #[default]
    NewMail,
    /// A scheduled follow-up came due.
    FollowUpDue,
}

/// The signals a [`PolicyGuard`](crate) reasons over when evaluating an action plan.
///
/// Constructed at the planning edge from the message, its classification, and the matched
/// human hard rules. `Default` makes the common "nothing special" context terse.
///
/// [`PolicyGuard`]: crate
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PolicyContext {
    /// The message the plan concerns, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    /// Categories the message was classified into.
    #[serde(default)]
    pub categories: Vec<MailCategory>,
    /// Whether the plan is a direct manual user override (the user asked for this).
    #[serde(default)]
    pub is_manual_user_override: bool,
    /// Whether a human hard rule explicitly allows moving this (possibly sensitive) mail.
    #[serde(default)]
    pub explicit_move_allowance: bool,
    /// What triggered planning.
    #[serde(default)]
    pub trigger: TriggerKind,
}

impl PolicyContext {
    /// Whether any of the message's categories is sensitive (financial/security/legal).
    #[must_use]
    pub fn has_sensitive_category(&self) -> bool {
        self.categories
            .iter()
            .copied()
            .any(MailCategory::is_sensitive)
    }
}

/// A record of one policy check that ran while evaluating a plan — the audit trail behind
/// a [`GuardedActionPlan`](crate::action::GuardedActionPlan).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PolicyCheckResult {
    /// The policy that produced this result (or a sentinel for an unrestricted action).
    pub policy_id: String,
    /// The outcome that policy reached for the action it examined.
    pub outcome: PolicyOutcome,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_labels_are_stable_and_round_trip() {
        assert_eq!(PolicyOutcome::Allowed.label(), "allowed");
        assert!(PolicyOutcome::Allowed.is_allowed());

        let review = PolicyOutcome::RequiresReview {
            reason: "bank alert".to_owned(),
        };
        assert_eq!(review.label(), "requires_review");
        assert!(!review.is_allowed());

        let blocked = PolicyOutcome::Blocked {
            policy_id: "never_auto_delete_mail".to_owned(),
            reason: "no".to_owned(),
        };
        let json = serde_json::to_value(&blocked).unwrap();
        assert_eq!(json["outcome"], "blocked");
        assert_eq!(json["policy_id"], "never_auto_delete_mail");
        let back: PolicyOutcome = serde_json::from_value(json).unwrap();
        assert_eq!(back, blocked);
    }

    #[test]
    fn sensitive_categories_are_flagged() {
        assert!(MailCategory::Financial.is_sensitive());
        assert!(MailCategory::Security.is_sensitive());
        assert!(MailCategory::Legal.is_sensitive());
        assert!(!MailCategory::General.is_sensitive());

        let ctx = PolicyContext {
            categories: vec![MailCategory::General, MailCategory::Security],
            ..PolicyContext::default()
        };
        assert!(ctx.has_sensitive_category());
        assert_eq!(PolicyContext::default().trigger, TriggerKind::NewMail);
    }
}
