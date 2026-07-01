//! The three-tier classification cascade (`ClassificationEngine`).
//!
//! Tier 1 — deterministic signals + classification rules (no model). If any **active**
//! classification rule fires, its labels/priority win and the message is accepted with zero
//! model inference (a learned trait that crystallized into a Tier-1 rule lives here).
//!
//! Tier 2 — the local [`Tier2Classifier`] over deterministic features. Accepted only when the
//! top-label confidence clears the band AND the phishing score is at or below the asymmetric
//! safety floor ("clearly safe" needs high confidence; "possibly phishing" escalates readily).
//!
//! Tier 3 — the LLM `classify_email` task, reached only on escalation. With **no provider
//! configured**, a message that needed Tier 3 degrades to `needs_review` — never an
//! auto-clear (MailMate stays safe with zero providers).

use std::sync::Arc;

use async_trait::async_trait;

use mailmate_ai::tasks::{classify_email, ClassifyEmailInput};
use mailmate_common::classification::{
    CascadeThresholds, Classification, ClassificationInput, ClassificationProvenance, Priority,
};
use mailmate_common::error::ClassificationError;
use mailmate_common::ids::RuleId;
use mailmate_common::mail::MessageData;
use mailmate_common::rules::evaluation::RuleEvaluationResult;
use mailmate_common::safety::assess_safety;
use mailmate_common::salient::{ai_signal, rule_signal, top_feature_signals};
use mailmate_ports::ai_provider::AiProvider;
use mailmate_ports::classification_engine::ClassificationEngine;
use mailmate_ports::rule_engine::RuleEngine;
use mailmate_ports::tier2_classifier::Tier2Classifier;

use crate::context::classification_context;

/// The default snippet budget (bytes) sent to a Tier-3 provider. A versioned prompt-template
/// parameter, not a hard constant — the egress posture caps content per feature.
const DEFAULT_SNIPPET_BUDGET: usize = 4096;

/// How many salient signals the cascade surfaces per verdict — the most-decisive few, so the
/// panel reads as reasons rather than a feature dump.
const MAX_SIGNALS: usize = 5;

/// The confidence reported for a Tier-3 (LLM) verdict. The provider does not emit a calibrated
/// probability, so the cascade reports a fixed *medium* band and leans on the honest
/// "AI assessment" salient signal for provenance — never a fabricated high confidence.
const TIER3_ASSESSED_CONFIDENCE: f64 = 0.7;

/// The cascade classifier: Pipeline 1's default `ClassificationEngine` adapter.
pub struct CascadeClassifier {
    classification_rules: Arc<dyn RuleEngine>,
    tier2: Arc<dyn Tier2Classifier>,
    provider: Option<Arc<dyn AiProvider>>,
    thresholds: CascadeThresholds,
    snippet_budget: usize,
}

impl CascadeClassifier {
    /// A cascade over a classification-rule engine and a Tier-2 model, with the default
    /// confidence bands and **no** Tier-3 provider (Tier-3-needed messages degrade to review).
    #[must_use]
    pub fn new(classification_rules: Arc<dyn RuleEngine>, tier2: Arc<dyn Tier2Classifier>) -> Self {
        Self {
            classification_rules,
            tier2,
            provider: None,
            thresholds: CascadeThresholds::default(),
            snippet_budget: DEFAULT_SNIPPET_BUDGET,
        }
    }

    /// Attach a Tier-3 provider for LLM escalation.
    #[must_use]
    pub fn with_provider(mut self, provider: Arc<dyn AiProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Override the (versioned) confidence bands.
    #[must_use]
    pub fn with_thresholds(mut self, thresholds: CascadeThresholds) -> Self {
        self.thresholds = thresholds;
        self
    }
}

#[async_trait]
impl ClassificationEngine for CascadeClassifier {
    async fn classify(
        &self,
        input: ClassificationInput,
    ) -> Result<Classification, ClassificationError> {
        let decision_id = input.decision_id.clone();

        // Inform-only Safety block — derived once from the message + features, independent of
        // which tier decides the verdict, and attached to whatever classification is returned.
        let safety = assess_safety(&input.message, &input.features);

        // --- Tier 1: deterministic signals + classification rules ---
        let ctx = classification_context(decision_id.clone(), &input.message, &input.features);
        let rule_result = self.classification_rules.evaluate(ctx).await?;
        if let Some(mut classification) = tier1_verdict(&decision_id, &rule_result) {
            classification.safety_findings = safety;
            return Ok(classification);
        }

        // --- Tier 2: the local model over deterministic features ---
        let scores = self.tier2.predict(input.features.clone()).await?;
        let (top_label, confidence) = top_score(&scores.scores);
        let spam_score = scores.scores.get("spam").copied().unwrap_or(0.0);
        let phishing_score = scores.scores.get("phishing").copied().unwrap_or(0.0);

        let confident = confidence >= self.thresholds.tier2_accept;
        let phishing_clear = phishing_score <= self.thresholds.phishing_safe_floor;
        if confident && phishing_clear {
            // The reasons the panel shows are the top signed contributions that actually produced
            // this score — correctable, because they are deterministic model features.
            let salient_signals = top_feature_signals(&scores.contributions, MAX_SIGNALS);
            return Ok(Classification {
                decision_id,
                labels: vec![top_label],
                spam_score,
                phishing_score,
                priority: Priority::Normal,
                needs_review: false,
                confidence,
                salient_signals,
                safety_findings: safety,
                provenance: ClassificationProvenance::tier2(scores.calibration_version, false),
            });
        }

        // --- Tier 3: LLM escalation (only when a provider is configured) ---
        if let Some(provider) = &self.provider {
            let response = classify_email(
                provider.as_ref(),
                ClassifyEmailInput {
                    from: input.message.headers.from.clone(),
                    subject: input.message.headers.subject.clone(),
                    snippet: bounded_snippet(&input.message, self.snippet_budget),
                },
            )
            .await?;
            // An LLM verdict is honestly an "AI assessment" — never dressed as a deterministic
            // signal, and not correctable as one.
            return Ok(Classification {
                decision_id,
                labels: response.labels,
                spam_score: f64::from(response.spam_score),
                phishing_score: f64::from(response.phishing_score),
                priority: response.priority,
                needs_review: false,
                confidence: TIER3_ASSESSED_CONFIDENCE,
                salient_signals: vec![ai_signal(provider.id().as_str())],
                safety_findings: safety,
                provenance: ClassificationProvenance::tier3(provider.id().to_string()),
            });
        }

        // --- No provider: a Tier-3-needed message degrades to review, never auto-cleared. ---
        // The Tier-2 contributions still explain *what the model saw* even though it abstained —
        // honest at cold-start ("here's what I can already see") rather than a blank review wall.
        let salient_signals = top_feature_signals(&scores.contributions, MAX_SIGNALS);
        Ok(Classification {
            decision_id,
            labels: vec!["needs_review".to_owned()],
            spam_score,
            phishing_score,
            priority: Priority::Normal,
            needs_review: true,
            confidence,
            salient_signals,
            safety_findings: safety,
            provenance: ClassificationProvenance::tier2(scores.calibration_version, true),
        })
    }
}

/// If any **active** classification rule contributed a classification effect (labels or
/// priority), build the Tier-1 verdict; otherwise `None` (fall through to Tier 2). Guarding on
/// a real classification effect keeps the cascade robust even if a mixed rule snapshot leaks an
/// action rule into this engine.
fn tier1_verdict(
    decision_id: &mailmate_common::ids::DecisionId,
    result: &RuleEvaluationResult,
) -> Option<Classification> {
    let mut labels: Vec<String> = Vec::new();
    let mut priority = Priority::Normal;
    let mut saw_classification_effect = false;

    for applied in &result.applied_effects {
        for label in &applied.effect.set_labels {
            if !labels.contains(label) {
                labels.push(label.clone());
            }
            saw_classification_effect = true;
        }
        if let Some(label) = &applied.effect.priority {
            priority = Priority::from_label(label);
            saw_classification_effect = true;
        }
    }

    if !saw_classification_effect {
        return None;
    }

    let fired_rules: Vec<RuleId> = result
        .matched_rules
        .iter()
        .filter(|matched| matched.applied)
        .map(|matched| matched.rule_id.clone())
        .collect();

    // Deterministic hard signals: a rule that labels "spam"/"phishing" pins the safety score.
    let spam_score = f64::from(labels.iter().any(|l| l == "spam"));
    let phishing_score = f64::from(labels.iter().any(|l| l == "phishing"));

    // Each fired rule is a salient signal: the user steers it by editing the rule, so it is
    // surfaced but not correctable as a one-off signal.
    let salient_signals = fired_rules
        .iter()
        .map(|rule_id| rule_signal(rule_id.as_str()))
        .collect();

    Some(Classification {
        decision_id: decision_id.clone(),
        labels,
        spam_score,
        phishing_score,
        priority,
        needs_review: false,
        // A deterministic rule fired — the verdict is certain.
        confidence: 1.0,
        salient_signals,
        // The caller (`classify`) fills the Safety block; tier1_verdict has no message/features.
        safety_findings: Vec::new(),
        provenance: ClassificationProvenance::tier1(fired_rules),
    })
}

/// The argmax `(label, probability)` over a calibrated score map. Deterministic: ties resolve
/// to the lexicographically-first label (the map iterates in sorted key order).
fn top_score(scores: &std::collections::BTreeMap<String, f64>) -> (String, f64) {
    scores
        .iter()
        .fold(
            None,
            |best: Option<(&String, f64)>, (label, &prob)| match best {
                Some((_, best_prob)) if best_prob >= prob => best,
                _ => Some((label, prob)),
            },
        )
        .map_or_else(
            || ("unknown".to_owned(), 0.0),
            |(label, prob)| (label.clone(), prob),
        )
}

/// A UTF-8-safe, length-bounded body snippet for a Tier-3 call. Empty when the message
/// carries no retained body.
fn bounded_snippet(message: &MessageData, budget: usize) -> String {
    let Some(body) = &message.body_text else {
        return String::new();
    };
    if body.len() <= budget {
        return body.clone();
    }
    let mut end = budget;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    body[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::ids::{AccountId, FolderId};
    use mailmate_common::mail::MessageHeaders;

    fn message_with_body(body: Option<&str>) -> MessageData {
        MessageData {
            id: None,
            client_message_id: "1".to_owned(),
            account_id: AccountId::from("acct_a"),
            folder_id: FolderId::from("folder_inbox"),
            thread_id: None,
            headers: MessageHeaders::default(),
            body_text: body.map(str::to_owned),
            attachments: vec![],
            remote_content_loaded: false,
            sender_seen_count: None,
            sender_in_address_book: None,
        }
    }

    #[test]
    fn top_score_is_deterministic_argmax_with_lexicographic_tie_break() {
        let mut scores = std::collections::BTreeMap::new();
        scores.insert("ham".to_owned(), 0.5);
        scores.insert("spam".to_owned(), 0.5);
        // Tie → first key in sorted order ("ham").
        assert_eq!(top_score(&scores), ("ham".to_owned(), 0.5));
        scores.insert("phishing".to_owned(), 0.9);
        assert_eq!(top_score(&scores), ("phishing".to_owned(), 0.9));
        assert_eq!(
            top_score(&std::collections::BTreeMap::new()),
            ("unknown".to_owned(), 0.0)
        );
    }

    #[test]
    fn bounded_snippet_truncates_on_a_char_boundary_and_handles_empty() {
        assert_eq!(bounded_snippet(&message_with_body(None), 10), "");
        // "é" is two bytes; a budget that lands mid-char backs up to a boundary.
        let msg = message_with_body(Some("aéb"));
        let snippet = bounded_snippet(&msg, 2);
        assert!(snippet == "a", "got {snippet:?}");
        assert_eq!(
            bounded_snippet(&message_with_body(Some("short")), 4096),
            "short"
        );
    }
}
