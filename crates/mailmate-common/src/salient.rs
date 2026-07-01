//! The explainability spine: typed, human-readable, optionally-correctable signals behind a
//! classification verdict.
//!
//! A [`SalientSignal`] is *fidelity-first*: a Tier-2 signal is built from the **actual** signed
//! contribution a feature made to the score (see [`feature_signal`]), not a post-hoc guess; a
//! Tier-1 signal names the rule that fired; a Tier-3 signal is honestly labelled an *AI
//! assessment* and is never dressed up as a deterministic feature. Marking a correctable signal
//! wrong is a first-class correction (`signal_marked_wrong`).
//!
//! The feature key → English map lives here (not beside the extractor) because it is the single
//! shared presentation vocabulary: the cascade builds signals from it and the dashboard explain
//! view renders the same words. The keys mirror `DeterministicFeatureExtractor`
//! (`mailmate-ml::features`); adding a feature there with no entry here degrades gracefully to a
//! humanised key, never a raw `snake_case` id.

use serde::{Deserialize, Serialize};

use crate::features::SignalContribution;

/// A coarse semantic grouping for a signal — drives iconography/grouping in the panel.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    /// Sender authentication (SPF/DKIM/DMARC).
    Authentication,
    /// Who the sender is / your relationship to them.
    Sender,
    /// The subject line.
    Subject,
    /// Attachments.
    Attachment,
    /// Bulk / mailing-list mail.
    ListMail,
    /// Conversation threading.
    Threading,
    /// A classification rule that fired.
    Rule,
    /// An LLM assessment.
    AiAssessment,
    /// Anything else.
    Other,
}

impl SignalKind {
    /// The stable snake_case label used on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Authentication => "authentication",
            Self::Sender => "sender",
            Self::Subject => "subject",
            Self::Attachment => "attachment",
            Self::ListMail => "list_mail",
            Self::Threading => "threading",
            Self::Rule => "rule",
            Self::AiAssessment => "ai_assessment",
            Self::Other => "other",
        }
    }
}

/// Where a signal came from — the provenance that decides whether it can be corrected.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalSource {
    /// A deterministic Tier-2 model feature (correctable: the user can mark it wrong).
    DeterministicFeature,
    /// A Tier-1 classification rule that fired (you correct it by editing the rule).
    ClassificationRule,
    /// A Tier-3 LLM assessment (never dressed as a deterministic signal).
    AiAssessment,
}

impl SignalSource {
    /// The stable snake_case label used on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DeterministicFeature => "deterministic_feature",
            Self::ClassificationRule => "classification_rule",
            Self::AiAssessment => "ai_assessment",
        }
    }
}

/// One human-readable reason behind a verdict.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SalientSignal {
    /// Stable identifier — the feature key or rule id. `signal_marked_wrong` echoes this back so
    /// the host knows which signal the user rejected.
    pub id: String,
    /// Human-readable label ("Sender failed SPF/DKIM/DMARC authentication") — never a raw id.
    pub label: String,
    /// The coarse semantic group.
    pub kind: SignalKind,
    /// Where it came from.
    pub source: SignalSource,
    /// Signed contribution **toward the assigned verdict**: positive supports the label, negative
    /// argues against it. For non-feature sources (rule/AI) it is the nominal `1.0`.
    pub weight: f64,
    /// Whether the user can mark this signal wrong. Deterministic features are correctable; a
    /// rule (edit the rule) and an AI assessment (not a deterministic claim) are not.
    pub correctable: bool,
}

/// Build the Tier-2 signal for one feature contribution, oriented toward the verdict. The
/// `signed_weight` is the feature's contribution to the score after the model's confidence
/// floor — already oriented toward the top label by the model — so a positive weight always
/// reads as "this supports the verdict".
#[must_use]
pub fn feature_signal(contribution: &SignalContribution) -> SalientSignal {
    let (label, kind) = humanize(&contribution.key, contribution.value);
    SalientSignal {
        id: contribution.key.clone(),
        label,
        kind,
        source: SignalSource::DeterministicFeature,
        weight: contribution.signed_weight,
        correctable: true,
    }
}

/// A Tier-1 signal: an **active** classification rule fired. Not correctable as a signal — the
/// user steers it by editing the rule (the Rules manager), so the panel routes "this is wrong"
/// to the rule, not to a `signal_marked_wrong`.
#[must_use]
pub fn rule_signal(rule_id: &str) -> SalientSignal {
    SalientSignal {
        id: rule_id.to_owned(),
        label: "Matched one of your active rules".to_owned(),
        kind: SignalKind::Rule,
        source: SignalSource::ClassificationRule,
        weight: 1.0,
        correctable: false,
    }
}

/// A Tier-3 signal: an honest "AI assessment" marker. Labelled as a model judgement and never
/// dressed up as a deterministic feature, so it is not correctable as a signal.
#[must_use]
pub fn ai_signal(provider_id: &str) -> SalientSignal {
    SalientSignal {
        id: "ai_assessment".to_owned(),
        label: format!("Assessed by the {provider_id} AI model"),
        kind: SignalKind::AiAssessment,
        source: SignalSource::AiAssessment,
        weight: 1.0,
        correctable: false,
    }
}

/// The top `k` feature signals by absolute contribution (most decisive first), dropping
/// negligible terms so the panel shows reasons that actually moved the score.
#[must_use]
pub fn top_feature_signals(contributions: &[SignalContribution], k: usize) -> Vec<SalientSignal> {
    let mut ranked: Vec<&SignalContribution> = contributions
        .iter()
        .filter(|c| c.signed_weight.abs() > NEGLIGIBLE)
        .collect();
    // Sort by |contribution| desc; ties break on key for determinism (replayable explanations).
    ranked.sort_by(|a, b| {
        b.signed_weight
            .abs()
            .partial_cmp(&a.signed_weight.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.key.cmp(&b.key))
    });
    ranked.into_iter().take(k).map(feature_signal).collect()
}

/// A contribution below this magnitude is treated as noise (no explanatory value).
const NEGLIGIBLE: f64 = 1e-6;

/// Map a feature key (a plain `name` or a one-hot `name=value`) and its numeric value to a
/// human label and kind. Unmapped keys degrade to a humanised form of the key — never a raw id.
fn humanize(key: &str, value: f64) -> (String, SignalKind) {
    // One-hot text features are encoded `name=value`; phrase them from the embedded value.
    if let Some((name, val)) = key.split_once('=') {
        return match name {
            "sender_domain" => (format!("Sender domain is {val}"), SignalKind::Sender),
            "sender_address" => (format!("Sender address is {val}"), SignalKind::Sender),
            "sender_display_name" => (
                format!("Sender name is \u{201c}{val}\u{201d}"),
                SignalKind::Sender,
            ),
            _ => (
                format!("{} is {val}", humanize_token(name)),
                SignalKind::Other,
            ),
        };
    }
    match key {
        "auth_fail" => (
            "Sender failed SPF/DKIM/DMARC authentication".to_owned(),
            SignalKind::Authentication,
        ),
        "auth_present" => (
            "Carries sender-authentication results".to_owned(),
            SignalKind::Authentication,
        ),
        "spf_pass" => (
            "Passed SPF authentication".to_owned(),
            SignalKind::Authentication,
        ),
        "dkim_pass" => ("Passed DKIM signing".to_owned(), SignalKind::Authentication),
        "dmarc_pass" => (
            "Passed DMARC alignment".to_owned(),
            SignalKind::Authentication,
        ),
        "reply_to_mismatch" => (
            "Reply-To points to a different domain than the sender".to_owned(),
            SignalKind::Sender,
        ),
        "display_name_spoofed" => (
            "Display name embeds a different email address (possible spoof)".to_owned(),
            SignalKind::Sender,
        ),
        "in_address_book" => (
            "Sender is in your address book".to_owned(),
            SignalKind::Sender,
        ),
        "sender_seen_count" => (
            format!("You have seen this sender {} time(s) before", value as i64),
            SignalKind::Sender,
        ),
        "is_list_mail" => (
            "Bulk/list mail (has a List-Id header)".to_owned(),
            SignalKind::ListMail,
        ),
        "has_unsubscribe" => (
            "Has a one-click unsubscribe link".to_owned(),
            SignalKind::ListMail,
        ),
        "is_bulk" => (
            "Marked as bulk mail (Precedence: bulk)".to_owned(),
            SignalKind::ListMail,
        ),
        "has_executable" => (
            "Has an executable/script attachment".to_owned(),
            SignalKind::Attachment,
        ),
        "has_archive" => (
            "Has an archive attachment".to_owned(),
            SignalKind::Attachment,
        ),
        "has_invoice_pdf" => (
            "Has an invoice/receipt PDF".to_owned(),
            SignalKind::Attachment,
        ),
        "has_calendar" => ("Has a calendar invite".to_owned(), SignalKind::Attachment),
        "has_attachments" => ("Has attachments".to_owned(), SignalKind::Attachment),
        "attachment_count" => (
            format!("{} attachment(s)", value as i64),
            SignalKind::Attachment,
        ),
        "subject_has_urgency" => (
            "Subject uses urgency/pressure language".to_owned(),
            SignalKind::Subject,
        ),
        "subject_is_reply" => (
            "Subject is a reply or forward".to_owned(),
            SignalKind::Subject,
        ),
        "subject_len" => (
            format!("Subject is {} characters", value as i64),
            SignalKind::Subject,
        ),
        "is_threaded" => (
            "Part of an existing conversation".to_owned(),
            SignalKind::Threading,
        ),
        "recipient_count" => (
            format!("Addressed to {} recipient(s)", value as i64),
            SignalKind::Subject,
        ),
        other => (humanize_token(other), SignalKind::Other),
    }
}

/// Turn a `snake_case` token into a readable, capitalised phrase — the never-a-raw-id fallback.
fn humanize_token(token: &str) -> String {
    let spaced = token.replace('_', " ");
    let mut chars = spaced.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contribution(key: &str, value: f64, signed_weight: f64) -> SignalContribution {
        SignalContribution {
            key: key.to_owned(),
            value,
            signed_weight,
        }
    }

    #[test]
    fn a_feature_signal_is_correctable_and_keeps_its_key_as_the_id() {
        let sig = feature_signal(&contribution("auth_fail", 1.0, 0.9));
        assert_eq!(sig.id, "auth_fail");
        assert_eq!(sig.label, "Sender failed SPF/DKIM/DMARC authentication");
        assert_eq!(sig.kind, SignalKind::Authentication);
        assert_eq!(sig.source, SignalSource::DeterministicFeature);
        assert!(sig.correctable, "a model feature can be marked wrong");
        assert!((sig.weight - 0.9).abs() < 1e-9);
    }

    #[test]
    fn a_one_hot_key_renders_its_embedded_value_not_a_raw_id() {
        let sig = feature_signal(&contribution("sender_domain=paypa1.com", 1.0, -0.7));
        assert_eq!(sig.label, "Sender domain is paypa1.com");
        assert_eq!(sig.kind, SignalKind::Sender);
        // A negative weight argues against the verdict; it is still surfaced.
        assert!(sig.weight < 0.0);
    }

    #[test]
    fn an_unmapped_key_degrades_to_a_humanised_phrase_never_a_raw_id() {
        let (label, kind) = humanize("some_new_feature", 0.0);
        assert_eq!(label, "Some new feature");
        assert_eq!(kind, SignalKind::Other);
        // And a one-hot of an unmapped name keeps the value readable.
        let sig = feature_signal(&contribution("mystery=42", 1.0, 0.3));
        assert_eq!(sig.label, "Mystery is 42");
    }

    #[test]
    fn top_signals_rank_by_absolute_contribution_and_drop_noise() {
        let contributions = vec![
            contribution("subject_len", 30.0, 0.01),
            contribution("auth_fail", 1.0, 0.9),
            contribution("is_list_mail", 1.0, -0.5),
            contribution("has_attachments", 1.0, 0.0), // negligible: dropped
        ];
        let top = top_feature_signals(&contributions, 2);
        assert_eq!(top.len(), 2);
        // Most decisive first: |0.9| then |-0.5|.
        assert_eq!(top[0].id, "auth_fail");
        assert_eq!(top[1].id, "is_list_mail");
        // The zero-contribution feature never appears, even with k headroom.
        let all = top_feature_signals(&contributions, 10);
        assert!(all.iter().all(|s| s.id != "has_attachments"));
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn numeric_features_phrase_their_value() {
        assert_eq!(
            feature_signal(&contribution("sender_seen_count", 5.0, 0.4)).label,
            "You have seen this sender 5 time(s) before"
        );
        assert_eq!(
            feature_signal(&contribution("subject_len", 128.0, 0.2)).label,
            "Subject is 128 characters"
        );
    }

    #[test]
    fn rule_and_ai_signals_are_honest_and_not_correctable() {
        let rule = rule_signal("rule_abc");
        assert_eq!(rule.id, "rule_abc");
        assert_eq!(rule.source, SignalSource::ClassificationRule);
        assert!(
            !rule.correctable,
            "a rule is steered by editing it, not a signal correction"
        );

        let ai = ai_signal("ollama");
        assert_eq!(ai.id, "ai_assessment");
        assert!(ai.label.contains("ollama"));
        assert_eq!(ai.kind, SignalKind::AiAssessment);
        assert!(
            !ai.correctable,
            "an AI assessment is never a deterministic claim"
        );
    }

    #[test]
    fn kind_and_source_labels_are_stable() {
        assert_eq!(SignalKind::Authentication.as_str(), "authentication");
        assert_eq!(SignalKind::AiAssessment.as_str(), "ai_assessment");
        assert_eq!(
            SignalSource::DeterministicFeature.as_str(),
            "deterministic_feature"
        );
        assert_eq!(
            SignalSource::ClassificationRule.as_str(),
            "classification_rule"
        );
    }

    #[test]
    fn a_signal_round_trips_on_the_wire() {
        let sig = SalientSignal {
            id: "auth_fail".to_owned(),
            label: "Sender failed SPF/DKIM/DMARC authentication".to_owned(),
            kind: SignalKind::Authentication,
            source: SignalSource::DeterministicFeature,
            weight: 0.9,
            correctable: true,
        };
        let json = serde_json::to_string(&sig).unwrap();
        let back: SalientSignal = serde_json::from_str(&json).unwrap();
        assert_eq!(back, sig);
    }
}
