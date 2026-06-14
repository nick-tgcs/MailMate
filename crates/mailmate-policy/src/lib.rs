//! The hard-policy guard: MailMate's deterministic safety chokepoint.
//!
//! [`HardPolicyGuard`] implements the seven required hard policies as model-free rules
//! (determinism-first — no model is ever consulted here). It partitions every candidate
//! [`ProposedAction`] into allowed / requires-review / blocked and records which policy
//! decided each. The guarantees:
//!
//! - Prohibited acts (delete, send, open-link, remote-content download, payment-detail
//!   trust) are **always blocked**, even under a manual user override — `manual override
//!   wins` cannot force a prohibited action.
//! - Moving financial / security / legal mail **requires review** unless a human hard rule
//!   explicitly allows it.
//! - A manual user override makes an otherwise-safe action **allowed** (e.g. it clears the
//!   sensitive-move review requirement), but never rescues a prohibited act.
//! - There is no send path: a [`ProposedAction::SendDraft`] is blocked, so a scheduled
//!   follow-up that only ever emits `CreateDraft` is covered by the existing floor.

use async_trait::async_trait;

use mailmate_common::action::{
    ActionPlan, BlockedAction, GuardedActionPlan, PlannedAction, ProposedAction,
};
use mailmate_common::error::PolicyError;
use mailmate_common::policy::{PolicyCheckResult, PolicyContext, PolicyOutcome};
use mailmate_ports::policy_guard::PolicyGuard;

/// The required hard-policy identifiers (stable strings used in outcomes and audit rows).
pub mod policy_ids {
    /// MailMate must never auto-delete mail.
    pub const NEVER_AUTO_DELETE_MAIL: &str = "never_auto_delete_mail";
    /// MailMate must never auto-send drafts.
    pub const NEVER_AUTO_SEND_DRAFTS: &str = "never_auto_send_drafts";
    /// MailMate must never open links from email.
    pub const NEVER_OPEN_LINKS: &str = "never_open_links";
    /// MailMate must never download remote content for classification.
    pub const NEVER_DOWNLOAD_REMOTE_CONTENT: &str =
        "never_download_remote_content_for_classification";
    /// MailMate must never automatically trust payment-detail changes.
    pub const NEVER_AUTO_TRUST_PAYMENT_DETAIL_CHANGES: &str =
        "never_auto_trust_payment_detail_changes";
    /// Financial/security/legal moves require review unless explicitly allowed.
    pub const FINANCIAL_SECURITY_LEGAL_MOVE_REQUIRES_REVIEW: &str =
        "financial_security_legal_move_requires_review";
    /// Manual user override always wins, except for prohibited actions.
    pub const MANUAL_USER_OVERRIDE_WINS: &str = "manual_user_override_wins";
    /// Sentinel for a safe action no hard policy restricts.
    pub const NO_RESTRICTION: &str = "no_hard_policy_restriction";
    /// Sentinel for an explicit `RequireReview` request.
    pub const REVIEW_REQUESTED: &str = "review_requested";
}

/// The deterministic guard. Stateless — it reasons only over the plan and its context.
#[derive(Clone, Copy, Debug, Default)]
pub struct HardPolicyGuard;

impl HardPolicyGuard {
    /// Construct the guard.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// One action's evaluation: the deciding policy plus its outcome.
struct Evaluation {
    policy_id: &'static str,
    outcome: PolicyOutcome,
}

fn blocked(policy_id: &'static str, reason: &str) -> Evaluation {
    Evaluation {
        policy_id,
        outcome: PolicyOutcome::Blocked {
            policy_id: policy_id.to_owned(),
            reason: reason.to_owned(),
        },
    }
}

fn allowed(policy_id: &'static str) -> Evaluation {
    Evaluation {
        policy_id,
        outcome: PolicyOutcome::Allowed,
    }
}

fn requires_review(policy_id: &'static str, reason: &str) -> Evaluation {
    Evaluation {
        policy_id,
        outcome: PolicyOutcome::RequiresReview {
            reason: reason.to_owned(),
        },
    }
}

/// Evaluate a single candidate action against the hard policies.
fn evaluate_one(action: &ProposedAction, context: &PolicyContext) -> Evaluation {
    use policy_ids as p;
    match action {
        // Prohibited acts — blocked regardless of any override or allowance.
        ProposedAction::Delete { .. } => blocked(
            p::NEVER_AUTO_DELETE_MAIL,
            "MailMate never auto-deletes mail",
        ),
        ProposedAction::SendDraft { .. } => blocked(
            p::NEVER_AUTO_SEND_DRAFTS,
            "MailMate never auto-sends drafts; drafts are review-required only",
        ),
        ProposedAction::OpenLink { .. } => {
            blocked(p::NEVER_OPEN_LINKS, "MailMate never opens links from email")
        }
        ProposedAction::DownloadRemoteContent { .. } => blocked(
            p::NEVER_DOWNLOAD_REMOTE_CONTENT,
            "MailMate never downloads remote content for classification",
        ),
        ProposedAction::TrustPaymentDetailChange { .. } => blocked(
            p::NEVER_AUTO_TRUST_PAYMENT_DETAIL_CHANGES,
            "MailMate never automatically trusts payment-detail changes",
        ),

        // An explicit request for review is always review-required.
        ProposedAction::RequireReview { .. } => requires_review(
            p::REVIEW_REQUESTED,
            "action explicitly requests human review",
        ),

        // A move is the one safe act that can require review (sensitive mail).
        ProposedAction::Move { .. } => {
            if context.is_manual_user_override {
                // Override wins for an otherwise-safe action.
                allowed(p::MANUAL_USER_OVERRIDE_WINS)
            } else if context.has_sensitive_category() && !context.explicit_move_allowance {
                requires_review(
                    p::FINANCIAL_SECURITY_LEGAL_MOVE_REQUIRES_REVIEW,
                    "moving financial/security/legal mail requires review unless a human \
                     hard rule allows it",
                )
            } else {
                allowed(p::NO_RESTRICTION)
            }
        }

        // The remaining safe acts are always allowed (the draft itself stays
        // requires_review = 1, which is a property of the draft, not of creating it).
        ProposedAction::Tag { .. }
        | ProposedAction::MarkJunk { .. }
        | ProposedAction::CreateDraft { .. } => {
            if context.is_manual_user_override {
                allowed(p::MANUAL_USER_OVERRIDE_WINS)
            } else {
                allowed(p::NO_RESTRICTION)
            }
        }
    }
}

#[async_trait]
impl PolicyGuard for HardPolicyGuard {
    async fn evaluate_action_plan(
        &self,
        context: PolicyContext,
        plan: ActionPlan,
    ) -> Result<GuardedActionPlan, PolicyError> {
        let mut allowed_actions = Vec::new();
        let mut review_required_actions = Vec::new();
        let mut blocked_actions = Vec::new();
        let mut policy_checks = Vec::new();

        for action in plan.actions {
            let evaluation = evaluate_one(&action, &context);
            policy_checks.push(PolicyCheckResult {
                policy_id: evaluation.policy_id.to_owned(),
                outcome: evaluation.outcome.clone(),
            });

            match evaluation.outcome {
                PolicyOutcome::Blocked { policy_id, reason } => {
                    blocked_actions.push(BlockedAction {
                        action,
                        policy_id,
                        reason,
                    });
                }
                PolicyOutcome::RequiresReview { .. } => {
                    push_safe(&mut review_required_actions, &mut blocked_actions, action);
                }
                PolicyOutcome::Allowed => match action.to_planned() {
                    Some(planned) => allowed_actions.push(planned),
                    // Defensive: a prohibited act should never reach Allowed; if it somehow
                    // did, fail safe by blocking it rather than applying it.
                    None => blocked_actions.push(failsafe_block(action)),
                },
            }
        }

        Ok(GuardedActionPlan {
            decision_id: plan.decision_id,
            allowed_actions,
            review_required_actions,
            blocked_actions,
            policy_checks,
        })
    }
}

/// Route a review-required outcome to the review list, failing safe if the action is
/// somehow not projectable onto a safe `PlannedAction` (the deciding reason is already
/// recorded in `policy_checks`).
fn push_safe(
    review: &mut Vec<PlannedAction>,
    blocked: &mut Vec<BlockedAction>,
    action: ProposedAction,
) {
    match action.to_planned() {
        Some(planned) => review.push(planned),
        None => blocked.push(failsafe_block(action)),
    }
}

fn failsafe_block(action: ProposedAction) -> BlockedAction {
    BlockedAction {
        action,
        policy_id: "failsafe_block".to_owned(),
        reason: "a prohibited action reached a non-blocking outcome; blocked to fail safe"
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    use mailmate_common::ids::{DecisionId, MessageId};
    use mailmate_common::policy::MailCategory;

    fn guard() -> HardPolicyGuard {
        HardPolicyGuard::new()
    }

    fn plan(actions: Vec<ProposedAction>) -> ActionPlan {
        ActionPlan {
            decision_id: DecisionId::fresh(),
            message_id: Some(MessageId::fresh()),
            actions,
        }
    }

    fn move_action() -> ProposedAction {
        ProposedAction::Move {
            message_id: MessageId::from("msg_1"),
            to_folder: "folder_archive".into(),
        }
    }

    #[test]
    fn sensitive_move_requires_review_without_allowance() {
        let ctx = PolicyContext {
            categories: vec![MailCategory::Security],
            ..PolicyContext::default()
        };
        let guarded =
            block_on(guard().evaluate_action_plan(ctx, plan(vec![move_action()]))).unwrap();
        assert_eq!(guarded.review_required_actions.len(), 1);
        assert!(guarded.allowed_actions.is_empty());
        assert_eq!(
            guarded.policy_checks[0].policy_id,
            policy_ids::FINANCIAL_SECURITY_LEGAL_MOVE_REQUIRES_REVIEW
        );
    }

    #[test]
    fn sensitive_move_allowed_with_explicit_human_allowance() {
        let ctx = PolicyContext {
            categories: vec![MailCategory::Financial],
            explicit_move_allowance: true,
            ..PolicyContext::default()
        };
        let guarded =
            block_on(guard().evaluate_action_plan(ctx, plan(vec![move_action()]))).unwrap();
        assert_eq!(guarded.allowed_actions.len(), 1);
        assert!(guarded.review_required_actions.is_empty());
    }

    #[test]
    fn manual_override_does_not_rescue_a_prohibited_action() {
        let ctx = PolicyContext {
            is_manual_user_override: true,
            ..PolicyContext::default()
        };
        let guarded = block_on(guard().evaluate_action_plan(
            ctx,
            plan(vec![ProposedAction::Delete {
                message_id: MessageId::from("msg_1"),
            }]),
        ))
        .unwrap();
        assert_eq!(guarded.blocked_actions.len(), 1);
        assert_eq!(
            guarded.blocked_actions[0].policy_id,
            policy_ids::NEVER_AUTO_DELETE_MAIL
        );
    }
}
