//! The required provider tests (architecture.md → Testing Strategy → Provider tests),
//! all deterministic via the mock provider — no Ollama or real provider needed.

use futures::executor::block_on;
use serde_json::json;

use mailmate_ai::providers::MockProvider;
use mailmate_ai::schemas::{
    ClassifyEmailResponse, DraftReplyResponse, ExtractTasksResponse, Priority,
    ProposeRulesResponse, ThreadSummaryResponse,
};
use mailmate_ai::validate_and_parse;
use mailmate_common::ai::{SamplingParams, StructuredRequest, StructuredResponse};
use mailmate_common::error::AiError;
use mailmate_ports::ai_provider::AiProvider;

fn req() -> StructuredRequest {
    StructuredRequest {
        messages: vec![],
        json_schema: None,
        grammar: None,
        sampling: SamplingParams::default(),
    }
}

fn run(value: serde_json::Value) -> StructuredResponse {
    let provider = MockProvider::returning_json("mock", value);
    block_on(provider.complete_structured(req())).unwrap()
}

#[test]
fn mock_classification_response() {
    let resp = run(json!({
        "labels": ["receipt"], "spam_score": 0.02, "phishing_score": 0.01, "priority": "low"
    }));
    let parsed: ClassifyEmailResponse = validate_and_parse(&resp).unwrap();
    assert_eq!(parsed.labels, vec!["receipt".to_owned()]);
    assert_eq!(parsed.priority, Priority::Low);
}

#[test]
fn mock_draft_response() {
    let resp = run(json!({
        "subject": "Re: Quote", "body": "Thanks for your enquiry.", "safety_notes": []
    }));
    let parsed: DraftReplyResponse = validate_and_parse(&resp).unwrap();
    assert_eq!(parsed.subject, "Re: Quote");
    assert!(!parsed.body.is_empty());
}

#[test]
fn mock_thread_summary() {
    let resp = run(json!({
        "summary": "Two messages about a quote.", "key_points": ["price", "timeline"]
    }));
    let parsed: ThreadSummaryResponse = validate_and_parse(&resp).unwrap();
    assert_eq!(parsed.key_points.len(), 2);
}

#[test]
fn mock_task_extraction() {
    let resp = run(json!({
        "tasks": [{ "description": "Send the quote", "due": "2026-07-01" }]
    }));
    let parsed: ExtractTasksResponse = validate_and_parse(&resp).unwrap();
    assert_eq!(parsed.tasks.len(), 1);
    assert_eq!(parsed.tasks[0].due.as_deref(), Some("2026-07-01"));
}

#[test]
fn mock_rule_proposal() {
    let resp = run(json!({
        "proposals": [{ "title": "Tag receipts", "description": "Tag vendor receipts." }],
        "rationale": "Repeated manual filing of receipts."
    }));
    let parsed: ProposeRulesResponse = validate_and_parse(&resp).unwrap();
    assert_eq!(parsed.proposals.len(), 1);
}

#[test]
fn invalid_response_is_rejected_and_never_drives_an_action() {
    // "Invalid JSON response rejected", schema sense: well-formed JSON that does not match
    // the task schema is rejected by validation; the caller records an audit event and
    // takes no action. (The other sense — text that is not valid JSON at all — is the codec
    // path below and is also exercised by the adapter tests.)
    let resp = run(json!({ "totally": "unexpected", "shape": true }));
    let err = validate_and_parse::<ClassifyEmailResponse>(&resp).unwrap_err();
    assert!(matches!(err, AiError::Validation(_)), "got {err:?}");
}

#[test]
fn unparseable_provider_text_is_a_codec_error() {
    // "Invalid JSON response rejected", codec sense: a weak backend that returns non-JSON
    // text fails closed at the parse boundary rather than fabricating a result.
    let err = StructuredResponse::from_raw_json("I will not answer in JSON.", "prompt_repair")
        .unwrap_err();
    assert!(matches!(err, AiError::Codec(_)), "got {err:?}");
}

#[test]
fn schema_valid_but_policy_invalid_response_is_rejected_by_a_later_layer() {
    use futures::executor::block_on as run_async;
    use mailmate_common::action::{ActionPlan, ProposedAction};
    use mailmate_common::ids::{DecisionId, DraftId};
    use mailmate_common::policy::PolicyContext;
    use mailmate_policy::{policy_ids, HardPolicyGuard};
    use mailmate_ports::policy_guard::PolicyGuard;

    // The provider returns a perfectly schema-valid draft (validation PASSES here)...
    let resp = run(
        json!({ "subject": "Re: Quote", "body": "Pay now via this link.", "safety_notes": [] }),
    );
    let parsed: DraftReplyResponse = validate_and_parse(&resp).unwrap();
    assert!(!parsed.body.is_empty(), "schema validation accepts it");

    // ...but if a downstream step tried to turn it into a send, the POLICY layer blocks it.
    let plan = ActionPlan {
        decision_id: DecisionId::fresh(),
        message_id: None,
        actions: vec![ProposedAction::SendDraft {
            draft_id: DraftId::fresh(),
        }],
        authored_by: Vec::new(),
    };
    let guarded =
        run_async(HardPolicyGuard::new().evaluate_action_plan(PolicyContext::default(), plan))
            .unwrap();
    assert_eq!(
        guarded.blocked_actions.len(),
        1,
        "a later layer rejects the unsafe action"
    );
    assert_eq!(
        guarded.blocked_actions[0].policy_id,
        policy_ids::NEVER_AUTO_SEND_DRAFTS
    );
}
