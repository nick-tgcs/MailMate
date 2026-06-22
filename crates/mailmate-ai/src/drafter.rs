//! The default [`ReplyDrafter`] adapter: the `draft_reply` task over the frozen `AiProvider`.
//!
//! This is where the `ReplyDraftRequest` vocabulary meets the model: it lowers the bounded,
//! retention-clamped request into a [`DraftReplyInput`], threads the caller's
//! `forbidden_commitments` into the model guidance, runs the validated `draft_reply` task,
//! and lifts the response back into a [`DraftedReply`]. It carries no review flag and no id —
//! the core's `DraftService` assigns those — so this adapter cannot produce something that
//! looks ready to send. The provider is injected as the port; no provider detail leaks here.

use std::sync::Arc;

use async_trait::async_trait;

use mailmate_common::error::AiError;
use mailmate_common::reply::{DraftedReply, ReplyDraftRequest};
use mailmate_ports::ai_provider::AiProvider;
use mailmate_ports::reply_drafter::ReplyDrafter;

use crate::tasks::{draft_reply, DraftReplyInput};

/// A [`ReplyDrafter`] backed by the `draft_reply` task over an injected provider.
pub struct TaskReplyDrafter {
    provider: Arc<dyn AiProvider>,
}

impl TaskReplyDrafter {
    /// Build the drafter over `provider`.
    #[must_use]
    pub fn new(provider: Arc<dyn AiProvider>) -> Self {
        Self { provider }
    }

    /// Fold the optional user instruction and the forbidden-commitment list into one guidance
    /// string the model sees. The forbidden list is always restated (even with no user
    /// instruction) so the model is told what it must not commit to on every draft.
    fn guidance(request: &ReplyDraftRequest) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(instruction) = &request.user_instruction {
            if !instruction.trim().is_empty() {
                parts.push(instruction.trim().to_owned());
            }
        }
        if !request.forbidden_commitments.is_empty() {
            parts.push(format!(
                "Do not make any commitment about: {}. State no such commitment even if asked.",
                request.forbidden_commitments.join(", ")
            ));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" "))
        }
    }
}

#[async_trait]
impl ReplyDrafter for TaskReplyDrafter {
    async fn draft(&self, request: ReplyDraftRequest) -> Result<DraftedReply, AiError> {
        let guidance = Self::guidance(&request);
        let input = DraftReplyInput {
            subject: request.subject,
            from: request.counterparty,
            excerpt: request.excerpt,
            guidance,
        };
        let response = draft_reply(self.provider.as_ref(), input).await?;
        Ok(DraftedReply {
            subject: response.subject,
            body: response.body,
            safety_notes: response.safety_notes,
            rationale: response.rationale,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use mailmate_common::ai::{
        ProviderCapabilities, ProviderId, StructuredRequest, StructuredResponse,
    };
    use std::sync::Mutex;

    /// A provider that returns a canned draft JSON and captures the last prompt it saw, so we
    /// can assert the forbidden commitments reached the model.
    struct CapturingProvider {
        last_user_prompt: Mutex<String>,
    }

    #[async_trait]
    impl AiProvider for CapturingProvider {
        fn id(&self) -> ProviderId {
            ProviderId::from("prov_capture")
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }
        async fn complete_structured(
            &self,
            request: StructuredRequest,
        ) -> Result<StructuredResponse, AiError> {
            let joined = request
                .messages
                .iter()
                .map(|m| m.content.clone())
                .collect::<Vec<_>>()
                .join("\n");
            *self.last_user_prompt.lock().unwrap() = joined;
            Ok(StructuredResponse {
                raw_text: String::new(),
                parsed_json: serde_json::json!({
                    "subject": "Re: Quote",
                    "body": "Hi,\n\nThanks for sending this over.",
                    "safety_notes": ["No dates, prices, or payment changes were added."],
                    "rationale": "Polite acknowledgement, no commitments — matches your usual tone."
                }),
                schema_validated_by: None,
            })
        }
    }

    #[test]
    fn it_drafts_and_threads_forbidden_commitments_into_the_prompt() {
        let provider = Arc::new(CapturingProvider {
            last_user_prompt: Mutex::new(String::new()),
        });
        let drafter = TaskReplyDrafter::new(provider.clone());
        let request = ReplyDraftRequest {
            user_instruction: Some("Politely ask for the corrected invoice.".to_owned()),
            forbidden_commitments: vec!["dates".to_owned(), "payment_changes".to_owned()],
            ..ReplyDraftRequest::new("Quote", "buyer@acme.test", "Could you re-send?")
        };
        let drafted = block_on(drafter.draft(request)).unwrap();
        assert_eq!(drafted.subject, "Re: Quote");
        assert_eq!(drafted.safety_notes.len(), 1);
        assert!(
            drafted.rationale.contains("matches your usual tone"),
            "the model's rationale must reach the DraftedReply: {:?}",
            drafted.rationale
        );

        let prompt = provider.last_user_prompt.lock().unwrap().clone();
        assert!(prompt.contains("Politely ask"));
        assert!(
            prompt.contains("payment_changes"),
            "forbidden list reached the model: {prompt}"
        );
    }

    #[test]
    fn guidance_is_none_when_there_is_nothing_to_say() {
        let request = ReplyDraftRequest::new("s", "c", "e");
        assert!(TaskReplyDrafter::guidance(&request).is_none());
    }

    #[test]
    fn guidance_restates_forbidden_list_even_without_a_user_instruction() {
        let request = ReplyDraftRequest {
            forbidden_commitments: vec!["prices".to_owned()],
            ..ReplyDraftRequest::new("s", "c", "e")
        };
        let g = TaskReplyDrafter::guidance(&request).unwrap();
        assert!(g.contains("prices"));
    }
}
