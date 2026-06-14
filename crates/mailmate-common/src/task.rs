//! The [`TaskRequest`] vocabulary: on-demand AI tasks the user triggers.
//!
//! Distinct from [`ProposedAction`](crate::action::ProposedAction) (outbound things
//! MailMate *applies*) and [`UserCorrection`](crate::correction::UserCorrection) (inbound
//! teaching). A task is a *request for AI work the user asked for* — summarize this thread,
//! draft a reply, extract the to-dos, explain why this was classified the way it was. Each
//! maps to a typed function in `mailmate-ai::tasks` (except `ExplainClassification`, which
//! is deterministic — it replays the rule decision, no model).

use serde::{Deserialize, Serialize};

use crate::ids::{DecisionId, MessageId, ThreadId};

/// An on-demand AI task the user triggers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "task", rename_all = "snake_case")]
pub enum TaskRequest {
    /// Summarize a whole thread.
    SummarizeThread {
        /// The thread to summarize.
        thread_id: ThreadId,
    },
    /// Extract actionable tasks from a message.
    ExtractTasks {
        /// The message to mine for tasks.
        message_id: MessageId,
    },
    /// Explain how a prior classification/action decision was reached. Deterministic — it
    /// replays the recorded rule decision and needs no provider.
    ExplainClassification {
        /// The decision to explain.
        decision_id: DecisionId,
    },
    /// Draft a reply to a message (always persisted review-required; never sent).
    DraftReply {
        /// The message to reply to.
        message_id: MessageId,
        /// Optional user guidance to steer the draft.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        guidance: Option<String>,
    },
}

impl TaskRequest {
    /// Whether fulfilling this task requires a configured AI provider. Only
    /// [`ExplainClassification`](TaskRequest::ExplainClassification) does not — it replays a
    /// recorded decision deterministically, so it works with **zero providers configured**.
    #[must_use]
    pub fn requires_provider(&self) -> bool {
        !matches!(self, Self::ExplainClassification { .. })
    }

    /// The stable snake_case task name (for audit rows and metrics).
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::SummarizeThread { .. } => "summarize_thread",
            Self::ExtractTasks { .. } => "extract_tasks",
            Self::ExplainClassification { .. } => "explain_classification",
            Self::DraftReply { .. } => "draft_reply",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_explain_runs_without_a_provider() {
        assert!(!TaskRequest::ExplainClassification {
            decision_id: DecisionId::from("dec_1"),
        }
        .requires_provider());
        for task in [
            TaskRequest::SummarizeThread {
                thread_id: ThreadId::from("thread_1"),
            },
            TaskRequest::ExtractTasks {
                message_id: MessageId::from("msg_1"),
            },
            TaskRequest::DraftReply {
                message_id: MessageId::from("msg_1"),
                guidance: None,
            },
        ] {
            assert!(task.requires_provider(), "{} needs a provider", task.name());
        }
    }

    #[test]
    fn task_request_is_tagged_in_json_and_round_trips() {
        let task = TaskRequest::DraftReply {
            message_id: MessageId::from("msg_7"),
            guidance: Some("be terse".to_owned()),
        };
        let value = serde_json::to_value(&task).unwrap();
        assert_eq!(value["task"], "draft_reply");
        assert_eq!(value["message_id"], "msg_7");
        let back: TaskRequest = serde_json::from_value(value).unwrap();
        assert_eq!(back, task);

        // Guidance is optional on the wire.
        let terse: TaskRequest =
            serde_json::from_value(json!({ "task": "extract_tasks", "message_id": "msg_1" }))
                .unwrap();
        assert_eq!(terse.name(), "extract_tasks");
    }
}
