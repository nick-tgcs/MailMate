//! Rendering a [`DatasetPlan`] into one of the four JSONL views.
//!
//! All four are provider-neutral and derive from the *same* internal examples:
//! supervised-fine-tuning (chat or Alpaca shape), preference (chosen vs rejected),
//! evaluation (frozen test cases with an expected output), and safety-counterexample
//! (forbidden behaviour with its flags). Each line carries privacy/safety metadata, so a
//! consumer can re-check the privacy level it is handling without re-deriving anything.

use serde_json::{json, Value};

use mailmate_common::error::ExportError;
use mailmate_common::training::{DatasetType, ExportFormat, TrainingExample, TrainingInput};

use crate::datasets::DatasetPlan;

/// The user-turn content: the instruction followed by any redacted context features, in a
/// stable order (the context map is already sorted). Shared with the evaluator so a request
/// is rendered identically at export and at evaluation time.
pub(crate) fn render_user_content(input: &TrainingInput) -> String {
    let mut content = input.instruction.clone();
    let ctx = &input.context_features;
    if let Some(domain) = &ctx.sender_domain {
        content.push_str(&format!("\nsender_domain: {domain}"));
    }
    if let Some(summary) = &ctx.thread_summary {
        content.push_str(&format!("\nthread_summary: {summary}"));
    }
    if !ctx.forbidden_commitments.is_empty() {
        content.push_str(&format!(
            "\nforbidden_commitments: {}",
            ctx.forbidden_commitments.join(", ")
        ));
    }
    for (key, value) in &ctx.extra {
        content.push_str(&format!("\n{key}: {value}"));
    }
    content
}

fn messages(example: &TrainingExample) -> Value {
    let mut msgs = Vec::new();
    if let Some(system) = &example.input.system {
        msgs.push(json!({"role": "system", "content": system}));
    }
    msgs.push(json!({"role": "user", "content": render_user_content(&example.input)}));
    Value::Array(msgs)
}

fn safety_flag_labels(example: &TrainingExample) -> Vec<&'static str> {
    example.safety_flags.iter().map(|f| f.as_str()).collect()
}

fn metadata(example: &TrainingExample) -> Value {
    json!({
        "privacy_level": example.privacy_level.as_str(),
        "safety_flags": safety_flag_labels(example),
        "source": {
            "kind": example.source_feedback.kind.as_str(),
            "id": example.source_feedback.id.as_str(),
        },
        "quality_score": example.quality_score,
    })
}

fn sft_chat_line(example: &TrainingExample) -> Option<Value> {
    let target = example.target_output()?;
    Some(json!({
        "task": example.task.as_str(),
        "messages": messages(example),
        "target": target.body,
        "metadata": metadata(example),
    }))
}

fn sft_alpaca_line(example: &TrainingExample) -> Option<Value> {
    let target = example.target_output()?;
    Some(json!({
        "task": example.task.as_str(),
        "instruction": example.input.instruction,
        "input": render_user_content(&example.input),
        "output": target.body,
        "metadata": metadata(example),
    }))
}

fn preference_line(example: &TrainingExample) -> Option<Value> {
    // chosen = the human-preferred output; rejected = the AI candidate the human changed.
    let chosen = example.user_corrected_output.as_ref()?;
    let rejected = example.candidate_output.as_ref()?;
    Some(json!({
        "task": example.task.as_str(),
        "messages": messages(example),
        "chosen": chosen.body,
        "rejected": rejected.body,
        "metadata": metadata(example),
    }))
}

fn evaluation_line(example: &TrainingExample) -> Value {
    json!({
        "task": example.task.as_str(),
        "messages": messages(example),
        "expected": example.target_output().map(|o| o.body.clone()),
        "label": example.label.as_str(),
        "metadata": metadata(example),
    })
}

fn safety_line(example: &TrainingExample) -> Value {
    json!({
        "task": example.task.as_str(),
        "messages": messages(example),
        "forbidden": example.candidate_output.as_ref().map(|o| o.body.clone()),
        "label": example.label.as_str(),
        "safety_flags": safety_flag_labels(example),
        "metadata": metadata(example),
    })
}

fn lines_for(plan: &DatasetPlan) -> Vec<Value> {
    let examples = &plan.examples;
    match plan.record.dataset_type {
        DatasetType::Sft => match plan.record.export_format {
            ExportFormat::Alpaca => examples.iter().filter_map(sft_alpaca_line).collect(),
            _ => examples.iter().filter_map(sft_chat_line).collect(),
        },
        DatasetType::Preference => examples.iter().filter_map(preference_line).collect(),
        DatasetType::Evaluation => examples.iter().map(evaluation_line).collect(),
        DatasetType::SafetyCounterexample => examples.iter().map(safety_line).collect(),
    }
}

/// Render `plan` to newline-delimited JSON (one example per line, no trailing newline).
///
/// # Errors
/// [`ExportError::Serialization`] if a line cannot be serialized (not expected for these
/// closed shapes, but surfaced rather than panicked on).
pub fn render_jsonl(plan: &DatasetPlan) -> Result<String, ExportError> {
    let mut out = String::new();
    for (i, line) in lines_for(plan).into_iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let rendered =
            serde_json::to_string(&line).map_err(|e| ExportError::Serialization(e.to_string()))?;
        out.push_str(&rendered);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::time::Timestamp;
    use mailmate_common::training::{DatasetType, ExportFormat, ExportPrivacyLevel};

    fn plan_with(
        dataset_type: DatasetType,
        format: ExportFormat,
        examples: Vec<TrainingExample>,
    ) -> DatasetPlan {
        use mailmate_common::ids::DatasetId;
        use mailmate_common::training::TrainingDatasetRecord;
        DatasetPlan {
            record: TrainingDatasetRecord {
                id: DatasetId::from("ds_1"),
                name: "d".to_owned(),
                dataset_type,
                base_model_family: None,
                example_ids_hash: "h".to_owned(),
                positive_count: 0,
                negative_count: 0,
                validation_count: 0,
                test_count: 0,
                privacy_level: ExportPrivacyLevel::Metadata,
                export_format: format,
                artifact_path: None,
                created_at: Timestamp::now(),
            },
            examples,
        }
    }

    fn ex(
        candidate: Option<&str>,
        corrected: Option<&str>,
        label: mailmate_common::training::TrainingLabel,
        flags: Vec<mailmate_common::training::SafetyFlag>,
    ) -> TrainingExample {
        use mailmate_common::evidence::EvidenceSourceKind;
        use mailmate_common::ids::FeedbackId;
        use mailmate_common::training::{
            CandidateOutput, ContextFeatures, SourceFeedbackRef, TrainingInput, TrainingTask,
        };
        TrainingExample {
            id: "trn_1".to_owned(),
            task: TrainingTask::DraftReply,
            source_feedback: SourceFeedbackRef {
                kind: EvidenceSourceKind::Draft,
                id: FeedbackId::from("drffb_1"),
            },
            privacy_level: ExportPrivacyLevel::Redacted,
            base_model_family: None,
            input: TrainingInput {
                system: Some("You are MailMate".to_owned()),
                instruction: "Draft a reply".to_owned(),
                context_features: ContextFeatures {
                    sender_domain: Some("example.com".to_owned()),
                    forbidden_commitments: vec!["prices".to_owned()],
                    ..ContextFeatures::default()
                },
            },
            candidate_output: candidate.map(CandidateOutput::new),
            user_corrected_output: corrected.map(CandidateOutput::new),
            label,
            polarity: label.polarity(),
            quality_score: 0.8,
            safety_flags: flags,
            created_at: Timestamp::now(),
        }
    }

    fn parse_lines(s: &str) -> Vec<Value> {
        if s.is_empty() {
            return vec![];
        }
        s.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn sft_chat_renders_system_user_and_target() {
        use mailmate_common::training::TrainingLabel;
        let plan = plan_with(
            DatasetType::Sft,
            ExportFormat::JsonlChat,
            vec![ex(
                Some("ai"),
                Some("fixed"),
                TrainingLabel::Corrected,
                vec![],
            )],
        );
        let lines = parse_lines(&render_jsonl(&plan).unwrap());
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!(line["task"], "draft_reply");
        assert_eq!(line["target"], "fixed", "target prefers the correction");
        let msgs = line["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");
        assert!(msgs[1]["content"]
            .as_str()
            .unwrap()
            .contains("sender_domain: example.com"));
        assert!(msgs[1]["content"]
            .as_str()
            .unwrap()
            .contains("forbidden_commitments: prices"));
        assert_eq!(line["metadata"]["privacy_level"], "redacted");
    }

    #[test]
    fn sft_alpaca_uses_instruction_input_output() {
        use mailmate_common::training::TrainingLabel;
        let plan = plan_with(
            DatasetType::Sft,
            ExportFormat::Alpaca,
            vec![ex(Some("ai"), None, TrainingLabel::Accepted, vec![])],
        );
        let lines = parse_lines(&render_jsonl(&plan).unwrap());
        assert_eq!(lines[0]["instruction"], "Draft a reply");
        assert_eq!(lines[0]["output"], "ai");
        assert!(
            lines[0].get("messages").is_none(),
            "alpaca shape has no messages array"
        );
    }

    #[test]
    fn preference_renders_chosen_and_rejected() {
        use mailmate_common::training::TrainingLabel;
        let plan = plan_with(
            DatasetType::Preference,
            ExportFormat::PreferenceJsonl,
            vec![ex(
                Some("rejected text"),
                Some("chosen text"),
                TrainingLabel::Corrected,
                vec![],
            )],
        );
        let lines = parse_lines(&render_jsonl(&plan).unwrap());
        assert_eq!(lines[0]["chosen"], "chosen text");
        assert_eq!(lines[0]["rejected"], "rejected text");
    }

    #[test]
    fn evaluation_renders_expected_and_label() {
        use mailmate_common::training::TrainingLabel;
        let plan = plan_with(
            DatasetType::Evaluation,
            ExportFormat::JsonlChat,
            vec![ex(
                Some("ai"),
                Some("gold"),
                TrainingLabel::Accepted,
                vec![],
            )],
        );
        let lines = parse_lines(&render_jsonl(&plan).unwrap());
        assert_eq!(lines[0]["expected"], "gold");
        assert_eq!(lines[0]["label"], "accepted");
    }

    #[test]
    fn safety_renders_forbidden_and_flags() {
        use mailmate_common::training::{SafetyFlag, TrainingLabel};
        let plan = plan_with(
            DatasetType::SafetyCounterexample,
            ExportFormat::JsonlChat,
            vec![ex(
                Some("I will change the payment details"),
                None,
                TrainingLabel::Unsafe,
                vec![SafetyFlag::PaymentChange],
            )],
        );
        let lines = parse_lines(&render_jsonl(&plan).unwrap());
        assert_eq!(lines[0]["forbidden"], "I will change the payment details");
        assert_eq!(lines[0]["safety_flags"][0], "payment_change");
        assert_eq!(lines[0]["label"], "unsafe");
    }

    #[test]
    fn empty_plan_renders_empty_string() {
        let plan = plan_with(DatasetType::Sft, ExportFormat::JsonlChat, vec![]);
        assert_eq!(render_jsonl(&plan).unwrap(), "");
    }
}
