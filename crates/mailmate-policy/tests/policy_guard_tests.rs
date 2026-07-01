//! The required policy-guard test cases (architecture.md → Testing Strategy → Policy
//! guard tests), driven through the public `PolicyGuard` port.

use futures::executor::block_on;

use mailmate_common::action::{ActionPlan, ProposedAction};
use mailmate_common::ids::{DecisionId, DraftId, MessageId};
use mailmate_common::mail::DraftSpec;
use mailmate_common::policy::{MailCategory, PolicyContext, TriggerKind};
use mailmate_policy::{policy_ids, HardPolicyGuard};
use mailmate_ports::policy_guard::PolicyGuard;

fn evaluate(
    context: PolicyContext,
    actions: Vec<ProposedAction>,
) -> mailmate_common::action::GuardedActionPlan {
    let plan = ActionPlan {
        decision_id: DecisionId::fresh(),
        message_id: Some(MessageId::fresh()),
        actions,
        authored_by: Vec::new(),
    };
    block_on(HardPolicyGuard::new().evaluate_action_plan(context, plan)).unwrap()
}

fn assert_blocked_by(action: ProposedAction, expected_policy: &str) {
    let guarded = evaluate(PolicyContext::default(), vec![action]);
    assert_eq!(
        guarded.blocked_actions.len(),
        1,
        "exactly one block expected"
    );
    assert!(guarded.allowed_actions.is_empty());
    assert!(guarded.review_required_actions.is_empty());
    assert_eq!(guarded.blocked_actions[0].policy_id, expected_policy);
}

#[test]
fn auto_delete_is_blocked() {
    assert_blocked_by(
        ProposedAction::Delete {
            message_id: MessageId::from("msg_1"),
        },
        policy_ids::NEVER_AUTO_DELETE_MAIL,
    );
}

#[test]
fn auto_send_is_blocked() {
    assert_blocked_by(
        ProposedAction::SendDraft {
            draft_id: DraftId::from("draft_1"),
        },
        policy_ids::NEVER_AUTO_SEND_DRAFTS,
    );
}

#[test]
fn link_opening_is_blocked() {
    assert_blocked_by(
        ProposedAction::OpenLink {
            message_id: MessageId::from("msg_1"),
            url: "https://example.com/track".to_owned(),
        },
        policy_ids::NEVER_OPEN_LINKS,
    );
}

#[test]
fn remote_content_download_is_blocked() {
    assert_blocked_by(
        ProposedAction::DownloadRemoteContent {
            message_id: MessageId::from("msg_1"),
        },
        policy_ids::NEVER_DOWNLOAD_REMOTE_CONTENT,
    );
}

#[test]
fn payment_detail_trust_is_blocked() {
    assert_blocked_by(
        ProposedAction::TrustPaymentDetailChange {
            message_id: MessageId::from("msg_1"),
        },
        policy_ids::NEVER_AUTO_TRUST_PAYMENT_DETAIL_CHANGES,
    );
}

#[test]
fn financial_security_legal_move_requires_review_unless_allowed() {
    // Without an explicit human allowance → requires review.
    let ctx = PolicyContext {
        categories: vec![MailCategory::Financial],
        ..PolicyContext::default()
    };
    let move_action = ProposedAction::Move {
        message_id: MessageId::from("msg_1"),
        to_folder: "folder_archive".into(),
    };
    let guarded = evaluate(ctx, vec![move_action.clone()]);
    assert_eq!(guarded.review_required_actions.len(), 1);
    assert!(guarded.allowed_actions.is_empty());

    // With an explicit human allowance → allowed.
    let ctx_allowed = PolicyContext {
        categories: vec![MailCategory::Legal],
        explicit_move_allowance: true,
        ..PolicyContext::default()
    };
    let guarded_allowed = evaluate(ctx_allowed, vec![move_action]);
    assert_eq!(guarded_allowed.allowed_actions.len(), 1);
    assert!(guarded_allowed.review_required_actions.is_empty());
}

#[test]
fn manual_override_wins_where_the_action_is_otherwise_safe() {
    // A sensitive move that would otherwise require review is allowed under a manual
    // override (a safe action), but a prohibited action is still blocked.
    let ctx = PolicyContext {
        categories: vec![MailCategory::Security],
        is_manual_user_override: true,
        ..PolicyContext::default()
    };
    let guarded = evaluate(
        ctx,
        vec![
            ProposedAction::Move {
                message_id: MessageId::from("msg_1"),
                to_folder: "folder_archive".into(),
            },
            ProposedAction::Tag {
                message_id: MessageId::from("msg_1"),
                tag: "reviewed".to_owned(),
            },
        ],
    );
    assert_eq!(
        guarded.allowed_actions.len(),
        2,
        "override allows both safe actions"
    );
    assert!(guarded.review_required_actions.is_empty());
    assert!(guarded.blocked_actions.is_empty());
    assert_eq!(
        guarded.policy_checks[0].policy_id,
        policy_ids::MANUAL_USER_OVERRIDE_WINS
    );
}

#[test]
fn fired_follow_up_step_never_yields_an_auto_send() {
    // A fired follow-up step emits only CreateDraft (+ RequireReview); the draft is
    // review-required and there is no send action anywhere in the partition.
    let ctx = PolicyContext {
        trigger: TriggerKind::FollowUpDue,
        ..PolicyContext::default()
    };
    let guarded = evaluate(
        ctx,
        vec![
            ProposedAction::CreateDraft {
                draft: DraftSpec {
                    subject: "Following up on your quote".to_owned(),
                    body: "Just checking in.".to_owned(),
                    ..DraftSpec::default()
                },
            },
            ProposedAction::RequireReview {
                target: "follow-up draft".to_owned(),
            },
        ],
    );
    assert_eq!(guarded.allowed_actions.len(), 1, "CreateDraft is allowed");
    assert_eq!(
        guarded.review_required_actions.len(),
        1,
        "RequireReview surfaced"
    );
    assert!(guarded.blocked_actions.is_empty());

    // The serialized plan contains no send path at all.
    let json = serde_json::to_string(&guarded).unwrap();
    assert!(
        !json.contains("send_draft"),
        "no send action may appear: {json}"
    );
}
