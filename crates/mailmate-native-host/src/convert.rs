//! Outbound translation: domain values → the wire payloads the extension consumes.
//!
//! The inverse of [`protocol_dto`](crate::protocol_dto). These builders produce the
//! `serde_json::Value` payloads the host puts inside a response or notification frame, in the
//! shapes documented in *Native Messaging Protocol*. Keeping the projection here (rather than
//! deriving `Serialize` on the domain types) lets the wire stay stable while the domain
//! evolves, and keeps the safe/blocked partition explicit on the wire.

use serde_json::{json, Value};

use mailmate_common::action::{BlockedAction, GuardedActionPlan, PlannedAction};
use mailmate_common::classification::Classification;
use mailmate_common::workflow::{FiredStep, NeedsAttentionItem};
use mailmate_core::PlanningOutcome;

/// The bounded classification view the wire carries.
#[must_use]
pub fn classification_json(classification: &Classification) -> Value {
    json!({
        "spam_score": classification.spam_score,
        "phishing_score": classification.phishing_score,
        "priority": classification.priority.as_str(),
        "labels": classification.labels,
        "needs_review": classification.needs_review,
    })
}

/// A safe action plus the policy outcome that admitted it (`allowed` / `requires_review`) and
/// its `apply_state` — the per-action lifecycle bucket the per-message panel renders from:
/// `suggest` (the user clicks Apply), `auto_applied` (a crystallized rule already ran it), or
/// `blocked`. `policy_outcome` answers "did a hard policy admit it?"; `apply_state` answers
/// "has it happened, or is it still an offer?" — the two are distinct (an `allowed` action is
/// only `auto_applied` once a crystallized rule drove it; on a manual classify nothing has run
/// yet, so every safe action is a `suggest`).
#[must_use]
pub fn suggested_action_json(
    action: &PlannedAction,
    policy_outcome: &str,
    apply_state: &str,
) -> Value {
    let mut value = serde_json::to_value(action).unwrap_or_else(|_| json!({}));
    if let Value::Object(map) = &mut value {
        map.insert("policy_outcome".to_owned(), json!(policy_outcome));
        map.insert("apply_state".to_owned(), json!(apply_state));
    }
    value
}

/// A blocked candidate plus the hard policy that refused it. Its `apply_state` is `blocked` —
/// the panel greys it out and shows the policy, never an Apply button.
#[must_use]
pub fn blocked_action_json(blocked: &BlockedAction) -> Value {
    json!({
        "action": blocked.action,
        "policy_id": blocked.policy_id,
        "reason": blocked.reason,
        "apply_state": "blocked",
    })
}

/// The `allowed` ∪ `review_required` actions as `suggested_actions` — the synchronous
/// classify view, where nothing has been applied yet, so every safe action is `apply_state:
/// "suggest"` (the user is in the loop). The `auto_applied` bucket appears only on the
/// background `classification_ready` path, never here.
#[must_use]
pub fn suggested_actions_json(plan: &GuardedActionPlan) -> Vec<Value> {
    plan.allowed_actions
        .iter()
        .map(|a| suggested_action_json(a, "allowed", "suggest"))
        .chain(
            plan.review_required_actions
                .iter()
                .map(|a| suggested_action_json(a, "requires_review", "suggest")),
        )
        .collect()
}

/// The explanation block: a one-line summary plus the policy checks that produced the
/// partition. The summary is deterministic, derived from the plan's shape.
#[must_use]
pub fn explanation_json(outcome: &PlanningOutcome) -> Value {
    let plan = &outcome.guarded_plan;
    let policy_checks: Vec<&str> = plan
        .policy_checks
        .iter()
        .map(|c| c.policy_id.as_str())
        .collect();
    let summary = format!(
        "{} allowed, {} need review, {} blocked.",
        plan.allowed_actions.len(),
        plan.review_required_actions.len(),
        plan.blocked_actions.len()
    );
    json!({
        "summary": summary,
        "labels": outcome.classification.labels,
        "policy_checks": policy_checks,
    })
}

/// The `classify_message` response payload (synchronous, user-initiated read).
#[must_use]
pub fn classify_response_payload(outcome: &PlanningOutcome, thunderbird_message_id: &str) -> Value {
    let plan = &outcome.guarded_plan;
    json!({
        "decision_id": plan.decision_id,
        "thunderbird_message_id": thunderbird_message_id,
        "classification": classification_json(&outcome.classification),
        "suggested_actions": suggested_actions_json(plan),
        "blocked_actions": plan.blocked_actions.iter().map(blocked_action_json).collect::<Vec<_>>(),
        "explanation": explanation_json(outcome),
    })
}

/// The `classification_ready` notification payload (background push). It additionally lists
/// the actions the host already applied, so the extension does not re-apply them, and echoes
/// the message's `subject`/`from` header metadata so the dashboard Review queue can label each
/// card with the real message identity (these are header metadata, always within `metadata`
/// retention — never body content).
#[must_use]
pub fn classification_ready_payload(
    outcome: &PlanningOutcome,
    thunderbird_message_id: &str,
    applied: &[PlannedAction],
    subject: &str,
    from: &str,
) -> Value {
    let plan = &outcome.guarded_plan;
    json!({
        "thunderbird_message_id": thunderbird_message_id,
        "decision_id": plan.decision_id,
        "headers": { "subject": subject, "from": from },
        "classification": classification_json(&outcome.classification),
        "applied_actions": applied.iter().map(|a| suggested_action_json(a, "allowed", "auto_applied")).collect::<Vec<_>>(),
        "review_required_actions": plan
            .review_required_actions
            .iter()
            .map(|a| suggested_action_json(a, "requires_review", "suggest"))
            .collect::<Vec<_>>(),
        "blocked_actions": plan.blocked_actions.iter().map(blocked_action_json).collect::<Vec<_>>(),
        "explanation": explanation_json(outcome),
    })
}

/// The `followup_draft_ready` notification payload (host → extension). A fired step's
/// review-required draft, surfaced for human confirmation — never sent on arrival (it
/// mirrors `classification_ready`). The `guarded_plan` view states the create-draft is
/// `requires_review`, matching the documented frame shape; `requires_review` on the draft is
/// pinned `true` by construction upstream.
#[must_use]
pub fn followup_draft_ready_payload(fired: &FiredStep) -> Value {
    json!({
        "workflow_instance_id": fired.workflow_instance_id,
        "pipeline_item_id": fired.pipeline_item_id,
        "thread_id": fired.thread_id,
        "step_index": fired.step_index,
        "coalesced_from_step_indexes": fired.coalesced_from,
        "draft": {
            "draft_id": fired.draft.draft_id,
            "subject": fired.draft.subject,
            "body": fired.draft.body,
            "requires_review": fired.draft.requires_human_review,
            "safety_notes": fired.draft.safety_notes,
        },
        "guarded_plan": { "actions": [{ "kind": "create_draft", "policy_outcome": "requires_review" }] },
        "explanation": {
            "summary": format!("Follow-up step {} on the tracked deal (review-required).", fired.step_index),
        },
    })
}

/// The `followup_needs_attention` notification payload (host → extension). No draft — the
/// item went stale past the abandon horizon; the user is nudged to decide manually.
#[must_use]
pub fn followup_needs_attention_payload(item: &NeedsAttentionItem) -> Value {
    json!({
        "workflow_instance_id": item.workflow_instance_id,
        "pipeline_item_id": item.pipeline_item_id,
        "reason": item.reason,
        "skipped_step_indexes": item.skipped_step_indexes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::action::ProposedAction;
    use mailmate_common::classification::{ClassificationProvenance, Priority};
    use mailmate_common::ids::{DecisionId, FolderId, MessageId};
    use mailmate_common::policy::{PolicyCheckResult, PolicyOutcome};

    fn outcome() -> PlanningOutcome {
        let classification = Classification {
            decision_id: DecisionId::from("dec_1"),
            labels: vec!["invoice".to_owned()],
            spam_score: 0.1,
            phishing_score: 0.0,
            priority: Priority::High,
            needs_review: true,
            provenance: ClassificationProvenance::tier1(vec![]),
        };
        let guarded_plan = GuardedActionPlan {
            decision_id: DecisionId::from("dec_1"),
            allowed_actions: vec![PlannedAction::Tag {
                message_id: MessageId::from("msg_1"),
                tag: "needs-review".to_owned(),
            }],
            review_required_actions: vec![PlannedAction::Move {
                message_id: MessageId::from("msg_1"),
                to_folder: FolderId::from("Receipts"),
            }],
            blocked_actions: vec![],
            policy_checks: vec![PolicyCheckResult {
                policy_id: "never_auto_delete_mail".to_owned(),
                outcome: PolicyOutcome::Allowed,
            }],
        };
        PlanningOutcome {
            classification,
            guarded_plan,
        }
    }

    #[test]
    fn classify_response_carries_classification_actions_and_explanation() {
        let payload = classify_response_payload(&outcome(), "tb_9");
        assert_eq!(payload["thunderbird_message_id"], "tb_9");
        assert_eq!(payload["classification"]["priority"], "high");
        assert_eq!(payload["suggested_actions"].as_array().unwrap().len(), 2);
        // The allowed tag is marked allowed; the move is requires_review.
        assert_eq!(payload["suggested_actions"][0]["kind"], "tag");
        assert_eq!(payload["suggested_actions"][0]["policy_outcome"], "allowed");
        assert_eq!(
            payload["suggested_actions"][1]["policy_outcome"],
            "requires_review"
        );
        // A manual classify has applied nothing: every safe action is a pending suggestion.
        assert_eq!(payload["suggested_actions"][0]["apply_state"], "suggest");
        assert_eq!(payload["suggested_actions"][1]["apply_state"], "suggest");
        assert_eq!(
            payload["explanation"]["policy_checks"][0],
            "never_auto_delete_mail"
        );
    }

    #[test]
    fn classification_ready_lists_applied_actions_separately() {
        let applied = vec![PlannedAction::Tag {
            message_id: MessageId::from("msg_1"),
            tag: "needs-review".to_owned(),
        }];
        let payload = classification_ready_payload(
            &outcome(),
            "tb_9",
            &applied,
            "Q3 invoice",
            "billing@acme.test",
        );
        // The header metadata rides along so the dashboard card shows the real message identity.
        assert_eq!(payload["headers"]["subject"], "Q3 invoice");
        assert_eq!(payload["headers"]["from"], "billing@acme.test");
        assert_eq!(payload["applied_actions"].as_array().unwrap().len(), 1);
        assert_eq!(payload["applied_actions"][0]["kind"], "tag");
        // An applied action is past-tense: auto_applied (Undo), never a pending suggestion.
        assert_eq!(payload["applied_actions"][0]["apply_state"], "auto_applied");
        assert_eq!(
            payload["review_required_actions"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            payload["review_required_actions"][0]["apply_state"],
            "suggest"
        );
    }

    #[test]
    fn blocked_actions_carry_the_policy_that_refused_them() {
        let blocked = BlockedAction {
            action: ProposedAction::Delete {
                message_id: MessageId::from("msg_1"),
            },
            policy_id: "never_auto_delete_mail".to_owned(),
            reason: "deletion is forbidden".to_owned(),
        };
        let value = blocked_action_json(&blocked);
        assert_eq!(value["policy_id"], "never_auto_delete_mail");
        assert_eq!(value["action"]["kind"], "delete");
        assert_eq!(value["apply_state"], "blocked");
    }

    #[test]
    fn followup_draft_ready_carries_a_review_required_draft_and_no_send() {
        use mailmate_common::ids::{DraftId, PipelineItemId, ThreadId, WorkflowInstanceId};
        use mailmate_common::reply::{DraftedReply, ReplyDraft};
        let fired = FiredStep {
            workflow_instance_id: WorkflowInstanceId::from("wfi_1"),
            pipeline_item_id: PipelineItemId::from("pli_1"),
            thread_id: ThreadId::from("thread_1"),
            step_index: 2,
            coalesced_from: vec![1],
            draft: ReplyDraft::from_drafted(
                DraftId::from("draft_9"),
                DraftedReply::new("Re: Acme quote", "Checking in."),
            ),
        };
        let payload = followup_draft_ready_payload(&fired);
        assert_eq!(payload["workflow_instance_id"], "wfi_1");
        assert_eq!(payload["step_index"], 2);
        assert_eq!(payload["coalesced_from_step_indexes"][0], 1);
        assert_eq!(payload["draft"]["requires_review"], true);
        // The generated body rides along so the review compose window opens populated, not blank.
        assert_eq!(payload["draft"]["body"], "Checking in.");
        assert_eq!(
            payload["guarded_plan"]["actions"][0]["policy_outcome"],
            "requires_review"
        );
        assert!(!serde_json::to_string(&payload)
            .unwrap()
            .contains("\"send\""));
    }

    #[test]
    fn followup_needs_attention_carries_the_reason_and_skipped_steps() {
        use mailmate_common::ids::{PipelineItemId, WorkflowInstanceId};
        let item = NeedsAttentionItem {
            workflow_instance_id: WorkflowInstanceId::from("wfi_2"),
            pipeline_item_id: PipelineItemId::from("pli_2"),
            reason: "stale_past_horizon".to_owned(),
            skipped_step_indexes: vec![1, 2, 3],
        };
        let payload = followup_needs_attention_payload(&item);
        assert_eq!(payload["reason"], "stale_past_horizon");
        assert_eq!(payload["skipped_step_indexes"].as_array().unwrap().len(), 3);
    }
}
