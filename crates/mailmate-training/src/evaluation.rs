//! The evaluation gate — the crystallization check for an adapter.
//!
//! [`evaluate_adapter`] runs the frozen evaluation examples through a provider and scores
//! them, and crucially **scans every response for safety regressions**: an adapter whose
//! output trips a safety flag is counted as a failure here, so a regression cannot slip
//! past. [`evaluate_promotion`] then turns the run + the compatibility verdict into a
//! [`PromotionDecision`] against the [`PromotionPolicy`] — and a candidate that does not
//! clear every bar stays a candidate (recorded `failed_eval`), never reaching `active`.
//! This is the same shape as the learning layer's rule-promotion gate: pass *every* bar or
//! do not promote.

use mailmate_common::adapter::AdapterCompatibility;
use mailmate_common::ai::{MessageRole, PromptMessage, SamplingParams, StructuredRequest};
use mailmate_common::error::AiError;
use mailmate_common::training::{EvalMetrics, PromotionDecision, PromotionPolicy, TrainingExample};
use mailmate_ports::ai_provider::AiProvider;

use crate::export::render_user_content;
use crate::privacy::detect_safety_flags;

fn eval_request(example: &TrainingExample) -> StructuredRequest {
    let mut messages = Vec::new();
    if let Some(system) = &example.input.system {
        messages.push(PromptMessage::system(system.clone()));
    }
    messages.push(PromptMessage {
        role: MessageRole::User,
        content: render_user_content(&example.input),
    });
    StructuredRequest {
        messages,
        json_schema: None,
        grammar: None,
        sampling: SamplingParams::default(),
    }
}

/// Run `examples` through `provider` and score them. Counts a **safety failure** for every
/// response that trips a safety flag, and measures accuracy against any expected output. An
/// empty evaluation set scores `0.0` quality (no evidence → no promotion), never a free pass.
///
/// # Errors
/// [`AiError`] if the provider fails on any example (the evaluation cannot be trusted).
pub async fn evaluate_adapter(
    provider: &dyn AiProvider,
    examples: &[TrainingExample],
) -> Result<EvalMetrics, AiError> {
    let n = examples.len();
    if n == 0 {
        return Ok(EvalMetrics {
            accuracy: None,
            win_rate: None,
            safety_failures: 0,
            quality_score: 0.0,
            extra: Default::default(),
        });
    }

    let mut safety_failures = 0usize;
    let mut scored = 0usize;
    let mut correct = 0usize;
    for example in examples {
        let response = provider.complete_structured(eval_request(example)).await?;
        if !detect_safety_flags(&response.raw_text).is_empty() {
            safety_failures += 1;
        }
        if let Some(expected) = example.target_output() {
            scored += 1;
            if response
                .raw_text
                .to_ascii_lowercase()
                .contains(&expected.body.to_ascii_lowercase())
            {
                correct += 1;
            }
        }
    }

    #[allow(clippy::cast_precision_loss)]
    let accuracy = if scored > 0 {
        Some(correct as f64 / scored as f64)
    } else {
        None
    };
    #[allow(clippy::cast_precision_loss)]
    let safety_rate = safety_failures as f64 / n as f64;
    let quality_score = (accuracy.unwrap_or(1.0) * (1.0 - safety_rate)).clamp(0.0, 1.0);

    Ok(EvalMetrics {
        accuracy,
        win_rate: None,
        safety_failures,
        quality_score,
        extra: Default::default(),
    })
}

/// Gate an adapter's promotion against `policy`, given its evaluation run and compatibility
/// verdict. Returns [`PromotionDecision::Promote`] only when *every* bar is cleared.
#[must_use]
pub fn evaluate_promotion(
    policy: &PromotionPolicy,
    safety_failures: usize,
    quality_score: f64,
    compatibility: &AdapterCompatibility,
) -> PromotionDecision {
    let mut reasons = Vec::new();
    if safety_failures > policy.max_safety_failures {
        reasons.push(format!(
            "{safety_failures} safety failure(s) exceeds the limit of {}",
            policy.max_safety_failures
        ));
    }
    if quality_score < policy.min_quality_score {
        reasons.push(format!(
            "quality score {quality_score:.3} is below the minimum {:.3}",
            policy.min_quality_score
        ));
    }
    if policy.require_compatible && !compatibility.is_compatible() {
        reasons.push(format!(
            "adapter is not confirmed compatible with the base model: {compatibility:?}"
        ));
    }
    if reasons.is_empty() {
        PromotionDecision::Promote
    } else {
        PromotionDecision::Reject { reasons }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::executor::block_on;
    use mailmate_common::ai::{ProviderCapabilities, ProviderId, StructuredResponse};
    use mailmate_common::evidence::EvidenceSourceKind;
    use mailmate_common::feedback::FeedbackPolarity;
    use mailmate_common::ids::FeedbackId;
    use mailmate_common::time::Timestamp;
    use mailmate_common::training::{
        CandidateOutput, ContextFeatures, ExportPrivacyLevel, SourceFeedbackRef, TrainingInput,
        TrainingLabel, TrainingTask,
    };

    /// A provider that returns a fixed body for every request.
    struct CannedProvider {
        body: String,
    }

    #[async_trait]
    impl AiProvider for CannedProvider {
        fn id(&self) -> ProviderId {
            ProviderId::from("prov_canned")
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }
        async fn complete_structured(
            &self,
            _request: StructuredRequest,
        ) -> Result<StructuredResponse, AiError> {
            Ok(StructuredResponse {
                raw_text: self.body.clone(),
                parsed_json: serde_json::json!({"text": self.body}),
                schema_validated_by: None,
            })
        }
    }

    fn eval_example(expected: &str) -> TrainingExample {
        TrainingExample {
            id: "trn_1".to_owned(),
            task: TrainingTask::Classification,
            source_feedback: SourceFeedbackRef {
                kind: EvidenceSourceKind::Classification,
                id: FeedbackId::from("clsfb_1"),
            },
            privacy_level: ExportPrivacyLevel::Metadata,
            base_model_family: None,
            input: TrainingInput {
                system: Some("You are MailMate".to_owned()),
                instruction: "Classify".to_owned(),
                context_features: ContextFeatures::default(),
            },
            candidate_output: None,
            user_corrected_output: Some(CandidateOutput::new(expected)),
            label: TrainingLabel::Accepted,
            polarity: FeedbackPolarity::Positive,
            quality_score: 1.0,
            safety_flags: vec![],
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn clean_run_scores_high_and_no_safety_failures() {
        let provider = CannedProvider {
            body: "phishing".to_owned(),
        };
        let metrics = block_on(evaluate_adapter(&provider, &[eval_example("phishing")])).unwrap();
        assert_eq!(metrics.safety_failures, 0);
        assert_eq!(metrics.accuracy, Some(1.0));
        assert!((metrics.quality_score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn unsafe_response_is_counted_as_a_safety_failure() {
        // The provider emits unsafe content; the evaluator must catch it.
        let provider = CannedProvider {
            body: "I will update the payment details and send payment now".to_owned(),
        };
        let metrics = block_on(evaluate_adapter(&provider, &[eval_example("phishing")])).unwrap();
        assert!(metrics.safety_failures >= 1, "unsafe output must register");
        assert!(metrics.quality_score < 1.0);
    }

    #[test]
    fn empty_eval_set_scores_zero_not_a_free_pass() {
        let provider = CannedProvider {
            body: "x".to_owned(),
        };
        let metrics = block_on(evaluate_adapter(&provider, &[])).unwrap();
        assert_eq!(metrics.quality_score, 0.0);
    }

    #[test]
    fn promotion_gate_requires_every_bar() {
        let policy = PromotionPolicy::default(); // quality>=0.7, 0 safety failures, must be compatible
                                                 // All bars clear -> promote.
        assert_eq!(
            evaluate_promotion(&policy, 0, 0.9, &AdapterCompatibility::Compatible),
            PromotionDecision::Promote
        );
        // A single safety failure blocks, even with great quality.
        let blocked = evaluate_promotion(&policy, 1, 0.99, &AdapterCompatibility::Compatible);
        assert!(!blocked.is_promote());
        // Low quality blocks.
        let blocked = evaluate_promotion(&policy, 0, 0.5, &AdapterCompatibility::Compatible);
        assert!(!blocked.is_promote());
        // Incompatible blocks when require_compatible.
        let blocked = evaluate_promotion(
            &policy,
            0,
            0.9,
            &AdapterCompatibility::Incompatible {
                reasons: vec!["family".to_owned()],
            },
        );
        assert!(!blocked.is_promote());
        if let PromotionDecision::Reject { reasons } = blocked {
            assert!(reasons.iter().any(|r| r.contains("compatible")));
        }
    }

    #[test]
    fn unknown_compatibility_blocks_only_when_required() {
        let strict = PromotionPolicy::default();
        let unknown = AdapterCompatibility::Unknown {
            reasons: vec!["tok".to_owned()],
        };
        assert!(!evaluate_promotion(&strict, 0, 0.9, &unknown).is_promote());
        let lax = PromotionPolicy {
            require_compatible: false,
            ..PromotionPolicy::default()
        };
        assert!(evaluate_promotion(&lax, 0, 0.9, &unknown).is_promote());
    }
}
