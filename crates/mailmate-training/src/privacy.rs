//! Redaction and privacy-ceiling enforcement at export time.
//!
//! Two guarantees live here. First, **nothing above the requested ceiling ever leaves**: an
//! example whose content is more sensitive than the export's [`ExportPrivacyLevel`] is
//! redacted *down* (Full → Redacted) or stripped to metadata (→ Metadata), never emitted
//! as-is. Second, a `Full` (raw-body) ceiling is only permitted when the user's
//! [`RetentionLevel`] actually retains bodies — so an export cannot conjure raw content the
//! user never consented to store.
//!
//! Redaction is deliberately dependency-free and deterministic (no regex engine in the leaf
//! domain crate): it scrubs email-shaped tokens and long digit runs (phone / card / account
//! numbers). It over-redacts rather than under-redacts — a date may be scrubbed as a number
//! — because for privacy the safe direction is to remove too much.

use mailmate_common::error::ExportError;
use mailmate_common::retention::RetentionLevel;
use mailmate_common::training::{CandidateOutput, ExportPrivacyLevel, SafetyFlag, TrainingExample};

const EMAIL_PLACEHOLDER: &str = "[email]";
const NUMBER_PLACEHOLDER: &str = "[redacted-number]";

/// Whether a char can appear inside an email local/domain part.
fn is_email_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-' | '@')
}

/// Whether a char can appear inside a phone / card / account number run. The separator set
/// includes `/` and `,` so a card/account number grouped with slashes or commas
/// (`4111/1111/1111/1111`, `4111,1111,1111,1111`) is treated as one run, not split below the
/// digit threshold.
fn is_number_char(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, ' ' | '-' | '(' | ')' | '+' | '.' | '/' | ',')
}

/// Replace email-shaped substrings with [`EMAIL_PLACEHOLDER`]. An email run is a maximal
/// span of email chars with an `@` that has a non-empty local part and domain part. The
/// domain is NOT required to contain a dot, so dotless intranet / localhost / UK-style
/// addresses (`user@localhost`, `a.b@c`) are scrubbed too — under-redacting any of these
/// would leak a real address.
fn redact_emails(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if let Some(at) = run.find('@') {
            // A local part before the `@` and a domain part after it — enough to be an
            // address, dot or no dot.
            if at > 0 && at + 1 < run.len() {
                out.push_str(EMAIL_PLACEHOLDER);
                run.clear();
                return;
            }
        }
        out.push_str(run);
        run.clear();
    };
    for c in input.chars() {
        if is_email_char(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Replace digit-heavy runs (≥7 digits) with [`NUMBER_PLACEHOLDER`], catching phone, card,
/// and account numbers however they are grouped.
fn redact_numbers(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        let digits = run.chars().filter(char::is_ascii_digit).count();
        if digits >= 7 {
            // Preserve any leading/trailing whitespace the run absorbed.
            let leading: String = run.chars().take_while(|c| *c == ' ').collect();
            let trailing: String = {
                let t: String = run.chars().rev().take_while(|c| *c == ' ').collect();
                t.chars().rev().collect()
            };
            out.push_str(&leading);
            out.push_str(NUMBER_PLACEHOLDER);
            out.push_str(&trailing);
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in input.chars() {
        if is_number_char(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Scrub PII (emails, long digit runs) from `input`. Deterministic and idempotent.
#[must_use]
pub fn redact_text(input: &str) -> String {
    redact_numbers(&redact_emails(input))
}

/// Deterministically detect forbidden-commitment / unsafe-content categories in `text`.
/// Keyword-driven and explicit, so safety detection stays auditable.
#[must_use]
pub fn detect_safety_flags(text: &str) -> Vec<SafetyFlag> {
    let lower = text.to_ascii_lowercase();
    let any = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));
    let mut flags = Vec::new();

    if any(&[
        "payment detail",
        "bank detail",
        "account number",
        "routing number",
        "update the payment",
        "change the payment",
        "new bank",
    ]) {
        flags.push(SafetyFlag::PaymentChange);
    }
    if any(&[
        "password",
        "api key",
        "api-key",
        "secret token",
        "credential",
        "private key",
    ]) {
        flags.push(SafetyFlag::CredentialDisclosure);
    }
    if any(&[
        "legally",
        "liable",
        "we guarantee",
        "i guarantee",
        "warrant that",
        "binding",
    ]) {
        flags.push(SafetyFlag::LegalPosition);
    }
    if any(&[
        "$",
        "the price is",
        "we will charge",
        "refund of",
        "total cost",
    ]) {
        flags.push(SafetyFlag::PriceCommitment);
    }
    if any(&[
        "deadline",
        "by friday",
        "by monday",
        "no later than",
        "due by",
    ]) {
        flags.push(SafetyFlag::DateCommitment);
    }
    // A commitment phrasing tied to a sensitive category is an unsupported commitment.
    let commits = any(&["i will ", "we will ", "i'll ", "we'll ", "i can confirm"]);
    let has_sensitive = flags.iter().any(|f| {
        matches!(
            f,
            SafetyFlag::PaymentChange | SafetyFlag::PriceCommitment | SafetyFlag::DateCommitment
        )
    });
    if commits && has_sensitive {
        flags.push(SafetyFlag::UnsupportedCommitment);
    }
    flags.sort();
    flags.dedup();
    flags
}

fn redact_output(output: &CandidateOutput) -> CandidateOutput {
    CandidateOutput::new(redact_text(&output.body))
}

/// Reduce `example` to at most `ceiling`. An example already within the ceiling is returned
/// unchanged; a `Full` example at a `Redacted` ceiling has its bodies scrubbed; any example
/// above a `Metadata` ceiling has its body content stripped entirely. Total — it never
/// errors, because the safe response to over-sensitive content is always to remove it.
#[must_use]
pub fn enforce_privacy(
    mut example: TrainingExample,
    ceiling: ExportPrivacyLevel,
) -> TrainingExample {
    if example.privacy_level <= ceiling {
        return example;
    }
    match ceiling {
        ExportPrivacyLevel::Full => example, // unreachable: nothing is above Full
        ExportPrivacyLevel::Redacted => {
            example.candidate_output = example.candidate_output.as_ref().map(redact_output);
            example.user_corrected_output =
                example.user_corrected_output.as_ref().map(redact_output);
            example.input.context_features.thread_summary = example
                .input
                .context_features
                .thread_summary
                .as_deref()
                .map(redact_text);
            example.privacy_level = ExportPrivacyLevel::Redacted;
            example
        }
        ExportPrivacyLevel::Metadata => {
            example.candidate_output = None;
            example.user_corrected_output = None;
            example.input.context_features.thread_summary = None;
            example.privacy_level = ExportPrivacyLevel::Metadata;
            example
        }
    }
}

/// Assert that a `Full` (raw-body) export ceiling is permitted by the user's retention
/// level. A `Metadata`/`Redacted` ceiling is always allowed; a `Full` ceiling requires the
/// retention level to actually retain bodies.
///
/// # Errors
/// [`ExportError::Privacy`] if a `Full` ceiling is requested while retention keeps no body.
pub fn assert_ceiling_allowed(
    ceiling: ExportPrivacyLevel,
    retention: RetentionLevel,
) -> Result<(), ExportError> {
    if ceiling == ExportPrivacyLevel::Full && !retention.retains_body() {
        return Err(ExportError::Privacy(format!(
            "a full-body export ceiling requires a body-retaining retention level, but it is `{}`",
            retention.as_str()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::evidence::EvidenceSourceKind;
    use mailmate_common::feedback::FeedbackPolarity;
    use mailmate_common::ids::FeedbackId;
    use mailmate_common::time::Timestamp;
    use mailmate_common::training::{
        ContextFeatures, SourceFeedbackRef, TrainingInput, TrainingLabel, TrainingTask,
    };

    #[test]
    fn redaction_scrubs_emails_and_long_numbers_and_is_idempotent() {
        let input = "Contact me at jane.doe@example.com or call +1 (555) 123-4567.";
        let red = redact_text(input);
        assert!(red.contains(EMAIL_PLACEHOLDER), "{red}");
        assert!(red.contains(NUMBER_PLACEHOLDER), "{red}");
        assert!(!red.contains("example.com"));
        assert!(!red.contains("123-4567"));
        assert_eq!(redact_text(&red), red, "idempotent");
    }

    #[test]
    fn redaction_keeps_short_numbers_and_plain_words() {
        let input = "Order v1.2 has 3 items in folder_receipts.";
        let red = redact_text(input);
        assert_eq!(red, input, "no PII, nothing changes");
    }

    #[test]
    fn card_number_with_spaces_is_redacted_as_one_run() {
        let red = redact_text("card 4111 1111 1111 1111 expires soon");
        assert!(red.contains(NUMBER_PLACEHOLDER));
        assert!(!red.contains("4111"));
        assert!(red.contains("expires soon"));
    }

    #[test]
    fn dotless_domain_emails_are_redacted() {
        // Intranet / localhost / UK-style addresses have no dot in the domain — they are
        // still real addresses and must not survive.
        for input in [
            "ping foo.bar@localhost now",
            "mail user@server here",
            "uk style a.b@c done",
        ] {
            let red = redact_text(input);
            assert!(red.contains(EMAIL_PLACEHOLDER), "{input:?} -> {red:?}");
            assert!(!red.contains('@'), "no address survives: {red:?}");
        }
        // A bare `@` or a one-sided fragment is NOT an address and is left alone.
        assert_eq!(redact_text("rate 5 @ noon"), "rate 5 @ noon");
        assert_eq!(redact_text("trailing user@ here"), "trailing user@ here");
        assert_eq!(redact_text("leading @host here"), "leading @host here");
    }

    #[test]
    fn slash_and_comma_grouped_card_numbers_are_redacted() {
        for input in [
            "card 4111/1111/1111/1111 expires",
            "card 4111,1111,1111,1111 expires",
        ] {
            let red = redact_text(input);
            assert!(red.contains(NUMBER_PLACEHOLDER), "{input:?} -> {red:?}");
            assert!(!red.contains("4111"), "no card digits survive: {red:?}");
            assert!(red.contains("expires"));
        }
        // A short comma-grouped figure (under the digit threshold) is left alone.
        assert_eq!(redact_text("items 1, 2, 3 left"), "items 1, 2, 3 left");
    }

    #[test]
    fn safety_detection_flags_known_categories() {
        let flags =
            detect_safety_flags("I will update the payment details and send payment today.");
        assert!(flags.contains(&SafetyFlag::PaymentChange));
        assert!(
            flags.contains(&SafetyFlag::UnsupportedCommitment),
            "commitment + payment"
        );

        assert!(
            detect_safety_flags("here is the api key").contains(&SafetyFlag::CredentialDisclosure)
        );
        assert!(detect_safety_flags("we guarantee this is legally binding")
            .contains(&SafetyFlag::LegalPosition));
        assert!(detect_safety_flags("the deadline is fixed").contains(&SafetyFlag::DateCommitment));
        assert!(detect_safety_flags("a refund of the total cost")
            .contains(&SafetyFlag::PriceCommitment));
        assert!(detect_safety_flags("Thanks, sounds good!").is_empty());
    }

    fn example_at(level: ExportPrivacyLevel, body: &str) -> TrainingExample {
        TrainingExample {
            id: "trn_x".to_owned(),
            task: TrainingTask::DraftReply,
            source_feedback: SourceFeedbackRef {
                kind: EvidenceSourceKind::Draft,
                id: FeedbackId::from("drffb_1"),
            },
            privacy_level: level,
            base_model_family: None,
            input: TrainingInput {
                system: None,
                instruction: "Draft a reply".to_owned(),
                context_features: ContextFeatures {
                    thread_summary: Some(format!("ctx {body}")),
                    ..ContextFeatures::default()
                },
            },
            candidate_output: Some(CandidateOutput::new(body)),
            user_corrected_output: Some(CandidateOutput::new(body)),
            label: TrainingLabel::Accepted,
            polarity: FeedbackPolarity::Positive,
            quality_score: 0.9,
            safety_flags: vec![],
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn within_ceiling_is_unchanged() {
        let ex = example_at(ExportPrivacyLevel::Metadata, "hi");
        let out = enforce_privacy(ex.clone(), ExportPrivacyLevel::Redacted);
        assert_eq!(
            out, ex,
            "metadata example at a redacted ceiling is unchanged"
        );
    }

    #[test]
    fn full_example_is_redacted_down_to_a_redacted_ceiling() {
        let ex = example_at(ExportPrivacyLevel::Full, "mail me at a@b.com");
        let out = enforce_privacy(ex, ExportPrivacyLevel::Redacted);
        assert_eq!(out.privacy_level, ExportPrivacyLevel::Redacted);
        assert!(out
            .candidate_output
            .unwrap()
            .body
            .contains(EMAIL_PLACEHOLDER));
        assert!(out
            .input
            .context_features
            .thread_summary
            .unwrap()
            .contains(EMAIL_PLACEHOLDER));
    }

    #[test]
    fn above_metadata_ceiling_strips_all_body_content() {
        let ex = example_at(ExportPrivacyLevel::Full, "sensitive body");
        let out = enforce_privacy(ex, ExportPrivacyLevel::Metadata);
        assert_eq!(out.privacy_level, ExportPrivacyLevel::Metadata);
        assert!(out.candidate_output.is_none());
        assert!(out.user_corrected_output.is_none());
        assert!(out.input.context_features.thread_summary.is_none());
    }

    #[test]
    fn full_ceiling_requires_body_retaining_retention() {
        assert!(
            assert_ceiling_allowed(ExportPrivacyLevel::Full, RetentionLevel::Metadata).is_err()
        );
        assert!(assert_ceiling_allowed(ExportPrivacyLevel::Full, RetentionLevel::Bodies).is_ok());
        assert!(
            assert_ceiling_allowed(ExportPrivacyLevel::Redacted, RetentionLevel::Metadata).is_ok()
        );
        assert!(
            assert_ceiling_allowed(ExportPrivacyLevel::Metadata, RetentionLevel::Metadata).is_ok()
        );
    }
}
