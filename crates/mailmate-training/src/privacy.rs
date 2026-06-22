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
//! domain crate): char-scanning passes, idempotent, ordered. It over-redacts rather than
//! under-redacts — a date may be scrubbed as a number — because for privacy the safe
//! direction is to remove too much.
//!
//! # Redaction coverage matrix (Phase 8)
//! `redact_text` (the full down-level scrub, applied at the `Redacted` ceiling) covers, in
//! pass order:
//! | Category | Detector | Placeholder |
//! |---|---|---|
//! | Credentials — OpenAI `sk-`, GitHub `ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_`/`github_pat_`, AWS `AKIA`/`ASIA`, JWT `eyJ….….…` | prefix + ≥16-char body / 3 base64url segments, substring (catches secrets embedded in URLs) | `[secret]` |
//! | Financial — IBAN (`GB29NWBK…`, contiguous **and** space-grouped) | 2 letters + 2 digits + 11–30 upper-alnum | `[iban]` |
//! | URLs — `http://`, `https://`, `www.` | scheme/`www.` run to whitespace, trailing punctuation trimmed | `[url]` |
//! | Email addresses (incl. dotless intranet/localhost) | local`@`domain run | `[email]` |
//! | Long digit runs — phone / card / account numbers (≥7 digits, any grouping) | digit-heavy run | `[redacted-number]` |
//!
//! **Credentials and IBANs are scrubbed at EVERY ceiling, including `Full`** ([`scrub_secrets`]):
//! a raw-body export the user consented to is about email *content*, never a place for an API
//! key or a bank account number to leave the machine. URLs / emails / long numbers are
//! content and survive a `Full` ceiling; they are scrubbed only when down-levelling to
//! `Redacted`.
//!
//! # NOT redacted (documented gaps — `Redacted` is a best-effort PII scrub, not anonymisation)
//! - Person / organisation **names**, street **addresses**, and free-text identifiers (no NER).
//! - **Short** numbers (< 7 digits): a 4-digit PIN, a 5-digit ZIP, a 6-digit OTP survive.
//! - Credentials with **no recognised prefix/shape** (bespoke tokens, base64 blobs that are
//!   not JWTs), and non-IBAN account formats (US routing/account pairs as plain words).
//! - Anything inside an **attachment** or an image (only text bodies are scrubbed).
//!
//! A `Full`-ceiling export retains everything in this list by design; only the `[secret]`/
//! `[iban]` classes are unconditional. The `Metadata` ceiling — the safe default — strips all
//! body content outright, so none of these gaps apply there.

use mailmate_common::error::ExportError;
use mailmate_common::retention::RetentionLevel;
use mailmate_common::training::{CandidateOutput, ExportPrivacyLevel, SafetyFlag, TrainingExample};

const EMAIL_PLACEHOLDER: &str = "[email]";
const NUMBER_PLACEHOLDER: &str = "[redacted-number]";
const SECRET_PLACEHOLDER: &str = "[secret]";
const IBAN_PLACEHOLDER: &str = "[iban]";
const URL_PLACEHOLDER: &str = "[url]";

/// Minimum body length after a credential prefix (`sk-`, `ghp_`, …) before it is treated as a
/// real key. Real tokens are long; this keeps `sk-` in `task-12` from matching.
const MIN_SECRET_BODY: usize = 16;

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

/// A char usable inside a secret body (`sk-…`, `ghp_…`, JWT segment).
fn is_secret_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-')
}

/// Does `chars[i..]` start with the literal `pat`?
fn matches_at(chars: &[char], i: usize, pat: &str) -> bool {
    let pat: Vec<char> = pat.chars().collect();
    i + pat.len() <= chars.len() && chars[i..i + pat.len()] == pat[..]
}

/// True when position `i` begins a fresh token (start of input, or the previous char is not
/// alphanumeric) — keeps prefix detectors from firing mid-word.
fn at_token_boundary(chars: &[char], i: usize) -> bool {
    i == 0 || !chars[i - 1].is_ascii_alphanumeric()
}

/// The GitHub token prefixes (`ghp_` personal, `gho_`/`ghu_`/`ghs_`/`ghr_` scoped, and the
/// fine-grained `github_pat_`).
const GH_PREFIXES: &[&str] = &["github_pat_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_"];
/// AWS access-key-id prefixes (long-term `AKIA`, temporary `ASIA`).
const AWS_PREFIXES: &[&str] = &["AKIA", "ASIA"];

/// If a credential (API key / token / JWT) starts at `chars[i]`, return its exclusive end.
fn match_credential(chars: &[char], i: usize) -> Option<usize> {
    if !at_token_boundary(chars, i) {
        return None;
    }
    let n = chars.len();
    // A JWT: `eyJ…` base64url . base64url . base64url (header.payload.signature).
    if matches_at(chars, i, "eyJ") {
        if let Some(end) = match_jwt(chars, i) {
            return Some(end);
        }
    }
    // AWS key ids: a fixed prefix then ≥16 upper-alnum chars.
    for p in AWS_PREFIXES {
        if matches_at(chars, i, p) {
            let start = i + p.chars().count();
            let mut j = start;
            while j < n && (chars[j].is_ascii_uppercase() || chars[j].is_ascii_digit()) {
                j += 1;
            }
            if j - start >= MIN_SECRET_BODY {
                return Some(j);
            }
        }
    }
    // Prefix tokens: `sk-` / `pk-` (provider keys) and the GitHub family, then ≥16 body chars.
    let prefixes = ["sk-", "sk_", "pk-", "pk_"];
    for p in prefixes.iter().copied().chain(GH_PREFIXES.iter().copied()) {
        if matches_at(chars, i, p) {
            let start = i + p.chars().count();
            let mut j = start;
            while j < n && is_secret_char(chars[j]) {
                j += 1;
            }
            if j - start >= MIN_SECRET_BODY {
                return Some(j);
            }
        }
    }
    None
}

/// Match a JWT starting at `chars[i]` (already known to begin `eyJ`): three `.`-separated
/// base64url segments. Returns the exclusive end of the third segment.
fn match_jwt(chars: &[char], i: usize) -> Option<usize> {
    let n = chars.len();
    let seg = |from: usize| {
        let mut j = from;
        while j < n && (chars[j].is_ascii_alphanumeric() || matches!(chars[j], '_' | '-')) {
            j += 1;
        }
        j
    };
    let e1 = seg(i);
    if e1 - i < 8 || e1 >= n || chars[e1] != '.' {
        return None;
    }
    let e2 = seg(e1 + 1);
    if e2 - (e1 + 1) < 8 || e2 >= n || chars[e2] != '.' {
        return None;
    }
    let e3 = seg(e2 + 1);
    if e3 - (e2 + 1) < 4 {
        return None;
    }
    Some(e3)
}

/// Replace credential-shaped substrings with [`SECRET_PLACEHOLDER`]. Substring (not just
/// token-prefix) so a key embedded in a URL query is caught too.
fn scrub_credentials(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some(end) = match_credential(&chars, i) {
            out.push_str(SECRET_PLACEHOLDER);
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// If an IBAN starts at `chars[start]` (2 letters, 2 digits, then 11–30 upper-alnum body),
/// return its exclusive end. Handles the contiguous machine form **and** the space-grouped
/// print form (`GB29 NWBK 6016 …`), counting only upper-alnum so prose breaks the match.
fn match_iban(chars: &[char], start: usize) -> Option<usize> {
    let n = chars.len();
    let is_body = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit();
    if start + 4 > n
        || !(chars[start].is_ascii_uppercase()
            && chars[start + 1].is_ascii_uppercase()
            && chars[start + 2].is_ascii_digit()
            && chars[start + 3].is_ascii_digit())
    {
        return None;
    }
    let mut j = start;
    let mut alnum = 0usize;
    let mut last_end = start;
    loop {
        let mut g = 0;
        while j < n && is_body(chars[j]) && alnum < 34 {
            j += 1;
            g += 1;
            alnum += 1;
        }
        if g == 0 {
            break;
        }
        last_end = j;
        // Continue across a single space only if a body char follows (group separator).
        if j + 1 < n && chars[j] == ' ' && is_body(chars[j + 1]) {
            j += 1;
        } else {
            break;
        }
    }
    let right_ok = last_end >= n || !is_body(chars[last_end]);
    ((15..=34).contains(&alnum) && right_ok).then_some(last_end)
}

/// Replace IBAN-shaped spans with [`IBAN_PLACEHOLDER`].
fn redact_ibans(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < chars.len() {
        if at_token_boundary(&chars, i) {
            if let Some(end) = match_iban(&chars, i) {
                out.push_str(IBAN_PLACEHOLDER);
                i = end;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Whether a char is sentence punctuation that should not be swallowed into a URL placeholder.
fn is_trailing_punct(c: char) -> bool {
    matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '>' | '"' | '\'')
}

/// If a URL starts at `chars[i]` (`http://`, `https://`, or a `www.` at a token boundary),
/// return its exclusive end with trailing sentence punctuation trimmed off.
fn match_url(chars: &[char], i: usize) -> Option<usize> {
    let n = chars.len();
    let scheme_len = if matches_at(chars, i, "https://") {
        8
    } else if matches_at(chars, i, "http://") {
        7
    } else if matches_at(chars, i, "www.") && at_token_boundary(chars, i) {
        4
    } else {
        return None;
    };
    let mut j = i + scheme_len;
    while j < n && !chars[j].is_whitespace() {
        j += 1;
    }
    while j > i + scheme_len && is_trailing_punct(chars[j - 1]) {
        j -= 1;
    }
    Some(j)
}

/// Replace URLs with [`URL_PLACEHOLDER`].
fn redact_urls(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some(end) = match_url(&chars, i) {
            out.push_str(URL_PLACEHOLDER);
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Scrub the categories that must NEVER leave the machine regardless of export ceiling:
/// credentials (API keys, tokens, JWTs) and IBANs. Applied even at a `Full` (raw-body)
/// ceiling, because a consented raw-body corpus is about email content, not secrets.
/// Deterministic and idempotent.
#[must_use]
pub fn scrub_secrets(input: &str) -> String {
    redact_ibans(&scrub_credentials(input))
}

/// Scrub PII from `input` for a `Redacted`-ceiling export: the full coverage matrix —
/// credentials and IBANs (always), then URLs, emails, and long digit runs. Deterministic and
/// idempotent. See the module-level matrix for exactly what is and is not covered.
#[must_use]
pub fn redact_text(input: &str) -> String {
    redact_numbers(&redact_emails(&redact_urls(&scrub_secrets(input))))
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

fn scrub_secrets_output(output: &CandidateOutput) -> CandidateOutput {
    CandidateOutput::new(scrub_secrets(&output.body))
}

/// Scrub credentials / IBANs from every body field of `example`, leaving content intact. The
/// unconditional secret floor applied at any ceiling, including `Full`.
fn scrub_example_secrets(mut example: TrainingExample) -> TrainingExample {
    example.candidate_output = example.candidate_output.as_ref().map(scrub_secrets_output);
    example.user_corrected_output = example
        .user_corrected_output
        .as_ref()
        .map(scrub_secrets_output);
    example.input.context_features.thread_summary = example
        .input
        .context_features
        .thread_summary
        .as_deref()
        .map(scrub_secrets);
    example
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
        // Within the requested ceiling — but credentials and IBANs are scrubbed at EVERY
        // ceiling, including `Full`: a consented raw-body corpus is about email content, never
        // a place for an API key or bank account number to leave the machine.
        return scrub_example_secrets(example);
    }
    match ceiling {
        ExportPrivacyLevel::Full => scrub_example_secrets(example), // unreachable: nothing is above Full
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

    #[test]
    fn credentials_are_scrubbed() {
        for (input, secret) in [
            ("token sk-abcdefGHIJKLMNOP1234567890 ok", "sk-abcdefGHIJKLMNOP"),
            ("gh ghp_ABCDEFGHIJKLMNOPQRSTuvwxyz0123 done", "ghp_ABCDEFGHIJKLMNOP"),
            (
                "fine github_pat_11ABCDEFG0aAAaAAaAaa_bbbbCCCCdddd here",
                "github_pat_11",
            ),
            ("aws AKIAIOSFODNN7EXAMPLE rotated", "AKIAIOSFODNN7EXAMPLE"),
            (
                "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w end",
                "eyJhbGci",
            ),
        ] {
            let red = redact_text(input);
            assert!(red.contains(SECRET_PLACEHOLDER), "{input:?} -> {red:?}");
            assert!(
                !red.contains(secret),
                "secret survived: {input:?} -> {red:?}"
            );
        }
        // A short `sk-` fragment in an ordinary word is NOT a key.
        assert_eq!(redact_text("the task-12 is due"), "the task-12 is due");
    }

    #[test]
    fn credentials_embedded_in_a_url_are_scrubbed_before_the_url() {
        // The credential floor runs before URL scrubbing, so the key never survives even when
        // it rides inside a query string. (`redact_text` then collapses the URL too.)
        let red = redact_text("see https://api.example.com/v1?token=ghp_ABCDEFGHIJKLMNOPqrstuvwx0 now");
        assert!(!red.contains("ghp_ABCDEFGHIJKLMNOP"), "{red:?}");
        // The whole thing is gone (secret scrubbed, then URL collapsed).
        assert!(red.contains(URL_PLACEHOLDER) || red.contains(SECRET_PLACEHOLDER), "{red:?}");
    }

    #[test]
    fn ibans_contiguous_and_space_grouped_are_redacted() {
        for input in [
            "pay to GB29NWBK60161331926819 today",
            "pay to GB29 NWBK 6016 1331 9268 19 today",
            "DE89 3704 0044 0532 0130 00 confirmed",
        ] {
            let red = redact_text(input);
            assert!(red.contains(IBAN_PLACEHOLDER), "{input:?} -> {red:?}");
            assert!(!red.contains("NWBK"), "{red:?}");
            assert!(!red.contains("3704"), "{red:?}");
        }
        // Not every "two letters two digits" prefix is an IBAN: a short code is left alone.
        assert_eq!(redact_text("ref GB29 ok"), "ref GB29 ok");
        // An all-caps phrase does not get eaten as an IBAN (lowercase breaks it).
        assert_eq!(redact_text("GB29 hello there"), "GB29 hello there");
    }

    #[test]
    fn urls_are_scrubbed_at_the_redacted_matrix() {
        for input in [
            "visit https://example.com/path?q=1 please",
            "visit http://example.com today",
            "visit www.example.com/page now",
        ] {
            let red = redact_text(input);
            assert!(red.contains(URL_PLACEHOLDER), "{input:?} -> {red:?}");
            assert!(!red.contains("example.com"), "{red:?}");
        }
        // Trailing sentence punctuation survives the placeholder.
        assert_eq!(
            redact_text("see https://x.io."),
            format!("see {URL_PLACEHOLDER}.")
        );
    }

    #[test]
    fn the_full_matrix_is_idempotent() {
        let input = "key sk-ABCDEFGHIJKLMNOP0123 at https://x.io/a?e=j@k.com pay GB29NWBK60161331926819 call +1 555 123 4567";
        let once = redact_text(input);
        assert_eq!(redact_text(&once), once, "idempotent: {once:?}");
    }

    #[test]
    fn a_full_ceiling_export_still_scrubs_credentials_and_ibans() {
        // The Phase-8 exit: a Full (raw-body) export cannot leave the machine carrying a
        // secret. Content (the greeting, the URL host) survives; the key and IBAN do not.
        let body = "Hi! Use sk-LIVEabcdefGHIJKLMN0123456789 and wire to GB29NWBK60161331926819. See https://example.com";
        let ex = example_at(ExportPrivacyLevel::Full, body);
        let out = enforce_privacy(ex, ExportPrivacyLevel::Full);
        assert_eq!(out.privacy_level, ExportPrivacyLevel::Full, "still a Full export");
        let scrubbed = out.candidate_output.unwrap().body;
        assert!(scrubbed.contains(SECRET_PLACEHOLDER), "{scrubbed:?}");
        assert!(scrubbed.contains(IBAN_PLACEHOLDER), "{scrubbed:?}");
        assert!(!scrubbed.contains("sk-LIVE"), "{scrubbed:?}");
        assert!(!scrubbed.contains("NWBK"), "{scrubbed:?}");
        // Full keeps content: the URL host is NOT scrubbed at a Full ceiling.
        assert!(scrubbed.contains("example.com"), "Full keeps content: {scrubbed:?}");
        assert!(scrubbed.contains("Hi!"), "{scrubbed:?}");
    }
}
