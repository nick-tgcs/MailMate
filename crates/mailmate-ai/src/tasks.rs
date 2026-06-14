//! The provider **task layer**: typed functions built on the single `complete_structured`
//! primitive, one per AI task. Each owns a *versioned* prompt template and an output schema,
//! sends the schema to schema-capable providers, and runs every response through
//! [`validate_and_parse`](crate::validate_and_parse) before returning a typed result.
//!
//! Adding a task touches **only this file** — never an adapter. The generative model always
//! runs frozen behind the `AiProvider` port; these functions are orchestration, not model
//! primitives. Egress posture (snippet caps, local-vs-remote routing) is enforced by the
//! caller that assembles the inputs; a task function only shapes and validates the call.

use serde_json::{json, Value};

use mailmate_common::ai::{PromptMessage, StructuredRequest};
use mailmate_common::error::AiError;
use mailmate_ports::ai_provider::AiProvider;

use crate::schemas::{
    ClassifyEmailResponse, DraftReplyResponse, ExtractTasksResponse, ThreadSummaryResponse,
};
use crate::validate_and_parse;

/// Versioned prompt-template ids — stamped into audit rows and evolved by the
/// prompt-evolution loop (so a regression is attributable to a template version).
pub const CLASSIFY_PROMPT_VERSION: &str = "classify-v1";
/// Draft-reply prompt-template version.
pub const DRAFT_PROMPT_VERSION: &str = "draft-v1";
/// Thread-summary prompt-template version.
pub const SUMMARIZE_PROMPT_VERSION: &str = "summarize-v1";
/// Task-extraction prompt-template version.
pub const EXTRACT_PROMPT_VERSION: &str = "extract-v1";

/// The bounded, pre-redacted context for a `classify_email` task. The caller is responsible
/// for honouring the egress posture (snippet caps, quoted-chain/signature stripping, local
/// vs. remote routing) when filling `snippet`.
#[derive(Clone, Debug, Default)]
pub struct ClassifyEmailInput {
    /// The `From` header.
    pub from: String,
    /// The subject line.
    pub subject: String,
    /// A bounded body snippet (may be empty when retention is `metadata`).
    pub snippet: String,
}

/// The context for a `draft_reply` task.
#[derive(Clone, Debug, Default)]
pub struct DraftReplyInput {
    /// The subject being replied to.
    pub subject: String,
    /// The counterparty address.
    pub from: String,
    /// A bounded excerpt of the message/thread to reply to.
    pub excerpt: String,
    /// Optional user guidance steering tone/content.
    pub guidance: Option<String>,
}

/// The context for a `summarize_thread` task.
#[derive(Clone, Debug, Default)]
pub struct SummarizeThreadInput {
    /// A bounded excerpt of the thread to summarize.
    pub excerpt: String,
}

/// The context for an `extract_tasks` task.
#[derive(Clone, Debug, Default)]
pub struct ExtractTasksInput {
    /// A bounded excerpt of the message to mine for tasks.
    pub excerpt: String,
}

/// Classify an email into labels + safety scores + priority (the Tier-3 cascade escalation).
///
/// # Errors
/// [`AiError`] on transport/enforcement failure, or [`AiError::Validation`] if the response
/// does not match [`ClassifyEmailResponse`]'s schema or semantic checks.
pub async fn classify_email(
    provider: &dyn AiProvider,
    input: ClassifyEmailInput,
) -> Result<ClassifyEmailResponse, AiError> {
    let messages = vec![
        PromptMessage::system(
            "You are MailMate's email classifier. Return ONLY JSON matching the schema: \
             labels (array of strings), spam_score and phishing_score (numbers in [0,1]), \
             and priority (one of low|normal|high|urgent). Do not include any prose.",
        ),
        PromptMessage::user(format!(
            "From: {}\nSubject: {}\n\n{}",
            input.from, input.subject, input.snippet
        )),
    ];
    let request = StructuredRequest::new(messages, classify_schema());
    let response = provider.complete_structured(request).await?;
    validate_and_parse(&response)
}

/// Draft a reply to a message. The result is advisory text only — it is always persisted
/// review-required downstream and can never itself be sent.
///
/// # Errors
/// [`AiError`] as for [`classify_email`], validated against [`DraftReplyResponse`].
pub async fn draft_reply(
    provider: &dyn AiProvider,
    input: DraftReplyInput,
) -> Result<DraftReplyResponse, AiError> {
    let mut user = format!(
        "Reply to this message.\nFrom: {}\nSubject: {}\n\n{}",
        input.from, input.subject, input.excerpt
    );
    if let Some(guidance) = &input.guidance {
        user.push_str(&format!("\n\nGuidance: {guidance}"));
    }
    let messages = vec![
        PromptMessage::system(
            "You are MailMate's reply drafter. Return ONLY JSON with: subject (string), \
             body (string), safety_notes (array of strings). The draft is for human review \
             and will never be sent automatically. Do not include any prose.",
        ),
        PromptMessage::user(user),
    ];
    let request = StructuredRequest::new(messages, draft_schema());
    let response = provider.complete_structured(request).await?;
    validate_and_parse(&response)
}

/// Summarize a thread into a short summary plus key points.
///
/// # Errors
/// [`AiError`] as for [`classify_email`], validated against [`ThreadSummaryResponse`].
pub async fn summarize_thread(
    provider: &dyn AiProvider,
    input: SummarizeThreadInput,
) -> Result<ThreadSummaryResponse, AiError> {
    let messages = vec![
        PromptMessage::system(
            "You are MailMate's thread summarizer. Return ONLY JSON with: summary (string), \
             key_points (array of strings). Do not include any prose.",
        ),
        PromptMessage::user(format!("Summarize this thread:\n\n{}", input.excerpt)),
    ];
    let request = StructuredRequest::new(messages, summarize_schema());
    let response = provider.complete_structured(request).await?;
    validate_and_parse(&response)
}

/// Extract actionable tasks (with optional due dates) from a message.
///
/// # Errors
/// [`AiError`] as for [`classify_email`], validated against [`ExtractTasksResponse`].
pub async fn extract_tasks(
    provider: &dyn AiProvider,
    input: ExtractTasksInput,
) -> Result<ExtractTasksResponse, AiError> {
    let messages = vec![
        PromptMessage::system(
            "You are MailMate's task extractor. Return ONLY JSON with: tasks (array of \
             objects each with description (string) and optional due (ISO-8601 string)). \
             Do not include any prose.",
        ),
        PromptMessage::user(format!(
            "Extract tasks from this message:\n\n{}",
            input.excerpt
        )),
    ];
    let request = StructuredRequest::new(messages, extract_schema());
    let response = provider.complete_structured(request).await?;
    validate_and_parse(&response)
}

/// The JSON schema sent to schema-capable providers for `classify_email`.
#[must_use]
pub fn classify_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["labels", "spam_score", "phishing_score", "priority"],
        "properties": {
            "labels": { "type": "array", "items": { "type": "string" } },
            "spam_score": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
            "phishing_score": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
            "priority": { "type": "string", "enum": ["low", "normal", "high", "urgent"] }
        }
    })
}

/// The JSON schema for `draft_reply`.
#[must_use]
pub fn draft_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["subject", "body"],
        "properties": {
            "subject": { "type": "string" },
            "body": { "type": "string" },
            "safety_notes": { "type": "array", "items": { "type": "string" } }
        }
    })
}

/// The JSON schema for `summarize_thread`.
#[must_use]
pub fn summarize_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["summary"],
        "properties": {
            "summary": { "type": "string" },
            "key_points": { "type": "array", "items": { "type": "string" } }
        }
    })
}

/// The JSON schema for `extract_tasks`.
#[must_use]
pub fn extract_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["tasks"],
        "properties": {
            "tasks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["description"],
                    "properties": {
                        "description": { "type": "string" },
                        "due": { "type": "string" }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use futures::executor::block_on;
    use serde_json::json;

    use mailmate_common::ai::{ProviderCapabilities, ProviderId, StructuredResponse};

    use super::*;
    use crate::schemas::Priority;

    /// A provider that records the last request it received and returns a programmed value.
    #[derive(Default)]
    struct CapturingProvider {
        last_request: Mutex<Option<StructuredRequest>>,
        response: Option<Value>,
        error: Option<AiError>,
    }

    impl CapturingProvider {
        fn returning(value: Value) -> Self {
            Self {
                response: Some(value),
                ..Self::default()
            }
        }
        fn failing(error: AiError) -> Self {
            Self {
                error: Some(error),
                ..Self::default()
            }
        }
    }

    #[async_trait]
    impl AiProvider for CapturingProvider {
        fn id(&self) -> ProviderId {
            ProviderId::from("capturing")
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                json_schema: true,
                ..ProviderCapabilities::default()
            }
        }
        async fn complete_structured(
            &self,
            request: StructuredRequest,
        ) -> Result<StructuredResponse, AiError> {
            *self.last_request.lock().unwrap() = Some(request);
            if let Some(error) = &self.error {
                return Err(error.clone());
            }
            let value = self.response.clone().unwrap();
            Ok(StructuredResponse {
                raw_text: value.to_string(),
                parsed_json: value,
                schema_validated_by: Some("capturing".to_owned()),
            })
        }
    }

    #[test]
    fn classify_email_builds_a_schema_request_and_returns_the_validated_result() {
        let provider = CapturingProvider::returning(json!({
            "labels": ["invoice"], "spam_score": 0.02, "phishing_score": 0.01, "priority": "high"
        }));
        let result = block_on(classify_email(
            &provider,
            ClassifyEmailInput {
                from: "billing@vendor.example".to_owned(),
                subject: "Invoice #42".to_owned(),
                snippet: "Please find attached.".to_owned(),
            },
        ))
        .unwrap();
        assert_eq!(result.labels, vec!["invoice".to_owned()]);
        assert_eq!(result.priority, Priority::High);

        // The request carried the schema and a system + user prompt with the snippet.
        let request = provider.last_request.lock().unwrap().clone().unwrap();
        assert!(request.json_schema.is_some(), "schema must be attached");
        assert_eq!(request.messages.len(), 2);
        assert!(request.messages[1].content.contains("Invoice #42"));
        assert!(request.messages[1]
            .content
            .contains("Please find attached."));
    }

    #[test]
    fn a_schema_mismatch_is_a_validation_error_that_never_drives_an_action() {
        let provider = CapturingProvider::returning(json!({ "totally": "unexpected" }));
        let err = block_on(classify_email(&provider, ClassifyEmailInput::default())).unwrap_err();
        assert!(matches!(err, AiError::Validation(_)), "got {err:?}");
    }

    #[test]
    fn a_transport_failure_propagates() {
        let provider = CapturingProvider::failing(AiError::RequestFailed {
            code: "503".to_owned(),
            message: "down".to_owned(),
        });
        let err =
            block_on(summarize_thread(&provider, SummarizeThreadInput::default())).unwrap_err();
        assert!(matches!(err, AiError::RequestFailed { .. }), "got {err:?}");
    }

    #[test]
    fn draft_reply_folds_in_guidance_and_validates() {
        let provider = CapturingProvider::returning(json!({
            "subject": "Re: Quote", "body": "Thanks — sending shortly.", "safety_notes": []
        }));
        let result = block_on(draft_reply(
            &provider,
            DraftReplyInput {
                subject: "Quote".to_owned(),
                from: "buyer@example.com".to_owned(),
                excerpt: "Can you send a quote?".to_owned(),
                guidance: Some("be warm".to_owned()),
            },
        ))
        .unwrap();
        assert_eq!(result.subject, "Re: Quote");
        let request = provider.last_request.lock().unwrap().clone().unwrap();
        assert!(request.messages[1].content.contains("Guidance: be warm"));
    }

    #[test]
    fn extract_tasks_returns_the_parsed_tasks() {
        let provider = CapturingProvider::returning(json!({
            "tasks": [{ "description": "Send the quote", "due": "2026-07-01" }]
        }));
        let result = block_on(extract_tasks(
            &provider,
            ExtractTasksInput {
                excerpt: "Please send the quote by July.".to_owned(),
            },
        ))
        .unwrap();
        assert_eq!(result.tasks.len(), 1);
        assert_eq!(result.tasks[0].due.as_deref(), Some("2026-07-01"));
    }

    #[test]
    fn schemas_are_well_formed_objects_with_required_fields() {
        for (schema, required) in [
            (classify_schema(), "spam_score"),
            (draft_schema(), "body"),
            (summarize_schema(), "summary"),
            (extract_schema(), "tasks"),
        ] {
            assert_eq!(schema["type"], "object");
            let required_list = schema["required"].as_array().unwrap();
            assert!(
                required_list.iter().any(|v| v == required),
                "{schema} must require {required}"
            );
        }
        // Prompt-template versions are stable, non-empty ids.
        assert_eq!(CLASSIFY_PROMPT_VERSION, "classify-v1");
        assert!(!DRAFT_PROMPT_VERSION.is_empty());
    }
}
