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
use mailmate_common::ids::RuleId;
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
    /// Never auto-junk or auto-file a reply in a conversation the user joined — the thread
    /// guard. Such an action is demoted to review (never auto-applied) unless the user is
    /// manually overriding.
    pub const NEVER_AUTO_ACT_ON_A_JOINED_THREAD: &str = "never_auto_act_on_a_joined_thread";
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
            } else if context.is_joined_thread {
                // The thread guard: never auto-file a reply in a conversation the user joined.
                requires_review(
                    p::NEVER_AUTO_ACT_ON_A_JOINED_THREAD,
                    "MailMate never auto-files a reply in a thread you joined; filing is \
                     review-required here",
                )
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

        // Junk-marking is normally a safe auto-act — except on a joined thread, where the thread
        // guard demotes an auto-junk to review (a legitimate conversation reply must never be
        // silently junked).
        ProposedAction::MarkJunk { .. } => {
            if context.is_manual_user_override {
                allowed(p::MANUAL_USER_OVERRIDE_WINS)
            } else if context.is_joined_thread {
                requires_review(
                    p::NEVER_AUTO_ACT_ON_A_JOINED_THREAD,
                    "MailMate never auto-junks a reply in a thread you joined; this is \
                     review-required",
                )
            } else {
                allowed(p::NO_RESTRICTION)
            }
        }

        // The remaining safe acts are always allowed (the draft itself stays
        // requires_review = 1, which is a property of the draft, not of creating it). Tagging a
        // joined thread is harmless (it adds a label, it does not file or junk), so it is not
        // gated by the thread guard.
        ProposedAction::Tag { .. } | ProposedAction::CreateDraft { .. } => {
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
        // Length-matched with `allowed_actions`: the authoring rule of each auto-applicable action,
        // carried through the partition so the apply path can stamp a rule's real fires. We push to
        // it in the SAME arm that pushes a `PlannedAction` onto `allowed_actions`, so the two never
        // drift. Provenance for review/blocked actions is dropped — only an allowed (auto-applied)
        // action can later be undone, so only its provenance feeds the undo-rate.
        let mut allowed_authored_by: Vec<Option<RuleId>> = Vec::new();
        let mut review_required_actions = Vec::new();
        let mut blocked_actions = Vec::new();
        let mut policy_checks = Vec::new();

        for (index, action) in plan.actions.into_iter().enumerate() {
            // Position-wise lookup, degrading a missing/short sidecar to `None` rather than panicking
            // (a plan built without provenance — e.g. a test literal — simply has none).
            let authored = plan.authored_by.get(index).cloned().flatten();
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
                    Some(planned) => {
                        allowed_actions.push(planned);
                        allowed_authored_by.push(authored);
                    }
                    // Defensive: a prohibited act should never reach Allowed; if it somehow
                    // did, fail safe by blocking it rather than applying it.
                    None => blocked_actions.push(failsafe_block(action)),
                },
            }
        }

        debug_assert_eq!(
            allowed_actions.len(),
            allowed_authored_by.len(),
            "allowed-action provenance sidecar must stay length-matched"
        );
        Ok(GuardedActionPlan {
            decision_id: plan.decision_id,
            allowed_actions,
            allowed_authored_by,
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
            authored_by: Vec::new(),
        }
    }

    fn move_action() -> ProposedAction {
        ProposedAction::Move {
            message_id: MessageId::from("msg_1"),
            to_folder: "folder_archive".into(),
        }
    }

    #[test]
    fn allowed_action_provenance_stays_aligned_when_a_sibling_is_demoted_to_review() {
        // The keystone's guard half: with a populated `authored_by` sidecar, the allowed-action
        // provenance must stay length-matched with `allowed_actions` even when an interleaved action
        // is demoted to review. Here Tag(rule_tag) is allowed and the sensitive Move(rule_move) is
        // demoted — so `allowed_authored_by` is exactly [rule_tag], not shifted onto the Move.
        use mailmate_common::ids::RuleId;
        let ctx = PolicyContext {
            categories: vec![MailCategory::Security],
            ..PolicyContext::default()
        };
        let plan = ActionPlan {
            decision_id: DecisionId::fresh(),
            message_id: Some(MessageId::from("msg_1")),
            actions: vec![
                ProposedAction::Tag {
                    message_id: MessageId::from("msg_1"),
                    tag: "flag".to_owned(),
                },
                move_action(),
            ],
            authored_by: vec![Some(RuleId::from("rule_tag")), Some(RuleId::from("rule_move"))],
        };
        let guarded = block_on(guard().evaluate_action_plan(ctx, plan)).unwrap();
        assert_eq!(guarded.allowed_actions.len(), 1, "only the Tag is allowed");
        assert!(matches!(guarded.allowed_actions[0], PlannedAction::Tag { .. }));
        assert_eq!(guarded.review_required_actions.len(), 1, "the sensitive Move is demoted");
        // The crux: the allowed sidecar carries the Tag's rule, not the demoted Move's — proving the
        // lockstep push happens only on the Allowed arm and never drifts off by an index.
        assert_eq!(
            guarded.allowed_authored_by,
            vec![Some(RuleId::from("rule_tag"))]
        );
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
    fn the_thread_guard_demotes_an_auto_junk_in_a_joined_thread_to_review() {
        // The §3.6 invariant: never auto-junk a reply in a thread the user joined.
        let ctx = PolicyContext {
            is_joined_thread: true,
            ..PolicyContext::default()
        };
        let guarded = block_on(guard().evaluate_action_plan(
            ctx,
            plan(vec![ProposedAction::MarkJunk {
                message_id: MessageId::from("msg_1"),
                junk: true,
            }]),
        ))
        .unwrap();
        // The junk is held for review, never auto-applied.
        assert!(guarded.allowed_actions.is_empty(), "auto-junk must not be allowed");
        assert_eq!(guarded.review_required_actions.len(), 1);
        assert_eq!(
            guarded.policy_checks[0].policy_id,
            policy_ids::NEVER_AUTO_ACT_ON_A_JOINED_THREAD
        );
    }

    #[test]
    fn the_thread_guard_also_demotes_an_auto_file_in_a_joined_thread() {
        let ctx = PolicyContext {
            is_joined_thread: true,
            ..PolicyContext::default()
        };
        let guarded =
            block_on(guard().evaluate_action_plan(ctx, plan(vec![move_action()]))).unwrap();
        assert!(guarded.allowed_actions.is_empty());
        assert_eq!(guarded.review_required_actions.len(), 1);
        assert_eq!(
            guarded.policy_checks[0].policy_id,
            policy_ids::NEVER_AUTO_ACT_ON_A_JOINED_THREAD
        );
    }

    #[test]
    fn a_manual_override_still_junks_a_joined_thread_when_the_user_asks() {
        // The guard blocks *auto* action, not the user's own deliberate one.
        let ctx = PolicyContext {
            is_joined_thread: true,
            is_manual_user_override: true,
            ..PolicyContext::default()
        };
        let guarded = block_on(guard().evaluate_action_plan(
            ctx,
            plan(vec![ProposedAction::MarkJunk {
                message_id: MessageId::from("msg_1"),
                junk: true,
            }]),
        ))
        .unwrap();
        assert_eq!(guarded.allowed_actions.len(), 1, "the user's explicit junk is allowed");
        assert!(guarded.review_required_actions.is_empty());
    }

    #[test]
    fn an_auto_junk_outside_a_joined_thread_is_still_allowed() {
        // A non-conversation message (e.g. a bulk sender) auto-junks as before — the guard is
        // narrow.
        let guarded = block_on(guard().evaluate_action_plan(
            PolicyContext::default(),
            plan(vec![ProposedAction::MarkJunk {
                message_id: MessageId::from("msg_1"),
                junk: true,
            }]),
        ))
        .unwrap();
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
