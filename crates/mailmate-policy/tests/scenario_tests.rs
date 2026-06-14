//! End-to-end-equivalent policy scenarios: realistic mixed plans evaluated whole through
//! the guard, asserting the full allowed/review/blocked partition and the audit trail —
//! the spec's worked examples (the bank-security-alert and the ordinary receipt).

use futures::executor::block_on;

use mailmate_common::action::{ActionPlan, ProposedAction};
use mailmate_common::ids::{DecisionId, MessageId};
use mailmate_common::policy::{MailCategory, PolicyContext};
use mailmate_policy::{policy_ids, HardPolicyGuard};
use mailmate_ports::policy_guard::PolicyGuard;

#[test]
fn bank_security_alert_plan_is_partitioned_correctly() {
    // A bank security alert: a rule proposes tagging it, archiving it (sensitive move, no
    // human allowance), and — adversarially — opening its verification link. The guard
    // must allow the tag, hold the move for review, and block the link.
    let ctx = PolicyContext {
        message_id: Some(MessageId::from("msg_bank")),
        categories: vec![MailCategory::Security, MailCategory::Financial],
        ..PolicyContext::default()
    };
    let plan = ActionPlan {
        decision_id: DecisionId::from("dec_bank"),
        message_id: Some(MessageId::from("msg_bank")),
        actions: vec![
            ProposedAction::Tag {
                message_id: MessageId::from("msg_bank"),
                tag: "security_alert".to_owned(),
            },
            ProposedAction::Move {
                message_id: MessageId::from("msg_bank"),
                to_folder: "folder_archive".into(),
            },
            ProposedAction::OpenLink {
                message_id: MessageId::from("msg_bank"),
                url: "https://totally-your-bank.example/verify".to_owned(),
            },
        ],
    };

    let guarded = block_on(HardPolicyGuard::new().evaluate_action_plan(ctx, plan)).unwrap();

    assert_eq!(guarded.allowed_actions.len(), 1, "tag is allowed");
    assert_eq!(
        guarded.review_required_actions.len(),
        1,
        "sensitive move held for review"
    );
    assert_eq!(guarded.blocked_actions.len(), 1, "link opening blocked");
    assert_eq!(
        guarded.blocked_actions[0].policy_id,
        policy_ids::NEVER_OPEN_LINKS
    );
    // Decision id is preserved end to end.
    assert_eq!(guarded.decision_id, DecisionId::from("dec_bank"));
    // One audit check per action.
    assert_eq!(guarded.policy_checks.len(), 3);
}

#[test]
fn ordinary_receipt_plan_is_fully_allowed() {
    // An ordinary software receipt (no sensitive category): tag + move are both allowed
    // without review.
    let ctx = PolicyContext {
        categories: vec![MailCategory::General],
        ..PolicyContext::default()
    };
    let plan = ActionPlan {
        decision_id: DecisionId::from("dec_receipt"),
        message_id: Some(MessageId::from("msg_receipt")),
        actions: vec![
            ProposedAction::Tag {
                message_id: MessageId::from("msg_receipt"),
                tag: "receipt".to_owned(),
            },
            ProposedAction::Move {
                message_id: MessageId::from("msg_receipt"),
                to_folder: "folder_receipts".into(),
            },
        ],
    };

    let guarded = block_on(HardPolicyGuard::new().evaluate_action_plan(ctx, plan)).unwrap();
    assert_eq!(guarded.allowed_actions.len(), 2);
    assert!(guarded.review_required_actions.is_empty());
    assert!(guarded.blocked_actions.is_empty());
}

#[test]
fn empty_plan_yields_an_empty_partition() {
    let guarded = block_on(HardPolicyGuard::new().evaluate_action_plan(
        PolicyContext::default(),
        ActionPlan {
            decision_id: DecisionId::from("dec_empty"),
            message_id: None,
            actions: vec![],
        },
    ))
    .unwrap();
    assert!(guarded.allowed_actions.is_empty());
    assert!(guarded.review_required_actions.is_empty());
    assert!(guarded.blocked_actions.is_empty());
    assert!(guarded.policy_checks.is_empty());
}
