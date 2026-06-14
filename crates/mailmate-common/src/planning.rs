//! The `ActionPlanner` port's input: everything Pipeline 2 needs to turn a classified
//! message into a candidate [`ActionPlan`](crate::action::ActionPlan).
//!
//! The planner stays **message-keyed** for every trigger. A `NewMail` trigger carries the
//! just-classified message; a `FollowUpDue` trigger carries the pipeline item's *anchor*
//! message (resolved by the scheduler from `thread_id` + `anchor_message_id`), so the
//! resulting draft/explanation/audit rows key off the anchor exactly like a reply would. No
//! second planner is introduced — only a new trigger.

use serde::{Deserialize, Serialize};

use crate::classification::Classification;
use crate::features::FeatureVector;
use crate::ids::{DecisionId, MessageId};
use crate::mail::MessageData;
use crate::policy::TriggerKind;

/// The input to `ActionPlanner::plan`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ActionPlanningInput {
    /// The decision this plan belongs to (shared with the classification that produced it).
    pub decision_id: DecisionId,
    /// What triggered planning (`NewMail` or `FollowUpDue`).
    pub trigger: TriggerKind,
    /// The anchor message the plan concerns.
    pub message: MessageData,
    /// Pipeline-1's verdict on the message — action rules key off these labels/scores.
    pub classification: Classification,
    /// The message's deterministic features (the same vector the cascade classified).
    pub features: FeatureVector,
}

impl ActionPlanningInput {
    /// Build a planning input, taking the decision id from the classification so the whole
    /// classify→plan→guard chain shares one decision identity.
    #[must_use]
    pub fn new(
        trigger: TriggerKind,
        message: MessageData,
        classification: Classification,
        features: FeatureVector,
    ) -> Self {
        Self {
            decision_id: classification.decision_id.clone(),
            trigger,
            message,
            classification,
            features,
        }
    }

    /// A new-mail planning input.
    #[must_use]
    pub fn new_mail(
        message: MessageData,
        classification: Classification,
        features: FeatureVector,
    ) -> Self {
        Self::new(TriggerKind::NewMail, message, classification, features)
    }

    /// A follow-up-due planning input (the `message` is the pipeline item's anchor).
    #[must_use]
    pub fn follow_up_due(
        message: MessageData,
        classification: Classification,
        features: FeatureVector,
    ) -> Self {
        Self::new(TriggerKind::FollowUpDue, message, classification, features)
    }

    /// The anchor message's resolved internal id, if it has been persisted. Planning that
    /// applies actions requires a stored message (the action targets an id).
    #[must_use]
    pub fn message_id(&self) -> Option<&MessageId> {
        self.message.id.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classification::{ClassificationProvenance, Priority};
    use crate::mail::{MessageData, MessageHeaders};

    fn message() -> MessageData {
        MessageData {
            id: Some(MessageId::from("msg_1")),
            client_message_id: "1".to_owned(),
            account_id: crate::ids::AccountId::from("acct_a"),
            folder_id: crate::ids::FolderId::from("folder_inbox"),
            thread_id: None,
            headers: MessageHeaders {
                from: "a@example.com".to_owned(),
                subject: "hi".to_owned(),
                ..MessageHeaders::default()
            },
            body_text: None,
            attachments: vec![],
            remote_content_loaded: false,
        }
    }

    fn classification() -> Classification {
        Classification {
            decision_id: DecisionId::from("dec_42"),
            labels: vec!["general".to_owned()],
            spam_score: 0.0,
            phishing_score: 0.0,
            priority: Priority::Normal,
            needs_review: false,
            provenance: ClassificationProvenance::tier1(vec![]),
        }
    }

    #[test]
    fn input_inherits_the_classification_decision_id_and_trigger() {
        let new_mail =
            ActionPlanningInput::new_mail(message(), classification(), FeatureVector::new());
        assert_eq!(new_mail.decision_id, DecisionId::from("dec_42"));
        assert_eq!(new_mail.trigger, TriggerKind::NewMail);
        assert_eq!(new_mail.message_id(), Some(&MessageId::from("msg_1")));

        let follow_up =
            ActionPlanningInput::follow_up_due(message(), classification(), FeatureVector::new());
        assert_eq!(follow_up.trigger, TriggerKind::FollowUpDue);
    }

    #[test]
    fn input_round_trips_through_serde() {
        let input =
            ActionPlanningInput::new_mail(message(), classification(), FeatureVector::new());
        let json = serde_json::to_string(&input).unwrap();
        let back: ActionPlanningInput = serde_json::from_str(&json).unwrap();
        assert_eq!(back, input);
    }
}
