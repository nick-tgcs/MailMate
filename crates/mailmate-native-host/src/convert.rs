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

/// A safe action plus the policy outcome that admitted it (`allowed` / `requires_review`).
#[must_use]
pub fn suggested_action_json(action: &PlannedAction, policy_outcome: &str) -> Value {
    let mut value = serde_json::to_value(action).unwrap_or_else(|_| json!({}));
    if let Value::Object(map) = &mut value {
        map.insert("policy_outcome".to_owned(), json!(policy_outcome));
    }
    value
}

/// A blocked candidate plus the hard policy that refused it.
#[must_use]
pub fn blocked_action_json(blocked: &BlockedAction) -> Value {
    json!({
        "action": blocked.action,
        "policy_id": blocked.policy_id,
        "reason": blocked.reason,
    })
}

/// The `allowed` ∪ `review_required` actions as `suggested_actions`, each tagged with its
/// policy outcome — the extension applies `allowed` ones and confirms `requires_review` ones.
#[must_use]
pub fn suggested_actions_json(plan: &GuardedActionPlan) -> Vec<Value> {
    plan.allowed_actions
        .iter()
        .map(|a| suggested_action_json(a, "allowed"))
        .chain(
            plan.review_required_actions
                .iter()
                .map(|a| suggested_action_json(a, "requires_review")),
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
/// the actions the host already applied, so the extension does not re-apply them.
#[must_use]
pub fn classification_ready_payload(
    outcome: &PlanningOutcome,
    thunderbird_message_id: &str,
    applied: &[PlannedAction],
) -> Value {
    let plan = &outcome.guarded_plan;
    json!({
        "thunderbird_message_id": thunderbird_message_id,
        "decision_id": plan.decision_id,
        "classification": classification_json(&outcome.classification),
        "applied_actions": applied.iter().map(|a| serde_json::to_value(a).unwrap_or(Value::Null)).collect::<Vec<_>>(),
        "review_required_actions": plan
            .review_required_actions
            .iter()
            .map(|a| suggested_action_json(a, "requires_review"))
            .collect::<Vec<_>>(),
        "blocked_actions": plan.blocked_actions.iter().map(blocked_action_json).collect::<Vec<_>>(),
        "explanation": explanation_json(outcome),
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
        let payload = classification_ready_payload(&outcome(), "tb_9", &applied);
        assert_eq!(payload["applied_actions"].as_array().unwrap().len(), 1);
        assert_eq!(payload["applied_actions"][0]["kind"], "tag");
        assert_eq!(
            payload["review_required_actions"].as_array().unwrap().len(),
            1
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
    }
}
