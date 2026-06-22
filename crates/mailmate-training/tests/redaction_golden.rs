//! Golden-corpus regression test — the Phase-8 HARD GATE on body redaction.
//!
//! Every row freezes a raw input against its exact redacted output. A change to `redact_text`
//! that drops a category, shifts a placeholder, or under-redacts a secret breaks this test and
//! fails the build (`-D warnings` + a red suite). This is the falsifiable contract behind the
//! claim "before training on bodies, the body passes redaction": if the corpus regresses, no
//! corpus ships.
//!
//! The cases also assert the *security invariant* directly: for any case tagged with a literal
//! secret, that literal must NOT appear in the output — independent of the exact placeholder.

use mailmate_training::privacy::{redact_text, scrub_secrets};

/// `(raw, expected_redacted_at_the_redacted_ceiling, must_not_survive)`.
/// `must_not_survive` is a substring that is a real secret/PII token and must be gone.
const GOLDEN: &[(&str, &str, &[&str])] = &[
    // --- Credentials -------------------------------------------------------------------
    (
        "Your OpenAI key is sk-proj-ABCdefGHIjklMNOpqrSTUvwx0123 keep it safe",
        "Your OpenAI key is [secret] keep it safe",
        &["sk-proj-ABCdefGHIjklMNOpqrSTUvwx0123"],
    ),
    (
        "deploy with ghp_ABCDEFGHIJKLMNOPqrstuvwxyz0123456789 in CI",
        "deploy with [secret] in CI",
        &["ghp_ABCDEFGHIJKLMNOPqrstuvwxyz0123456789"],
    ),
    (
        "fine-grained github_pat_11ABCDE0a_AAaaAAAaaaBBBBccccDDDDeeee set",
        "fine-grained [secret] set",
        &["github_pat_11ABCDE0a_AAaaAAAaaaBBBBccccDDDDeeee"],
    ),
    (
        "AWS key AKIAIOSFODNN7EXAMPLE and secret elsewhere",
        "AWS key [secret] and secret elsewhere",
        &["AKIAIOSFODNN7EXAMPLE"],
    ),
    (
        "session ASIAY34FZKBOKMUTVV7A expired",
        "session [secret] expired",
        &["ASIAY34FZKBOKMUTVV7A"],
    ),
    (
        "bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.SflKxwRJSMeKKF2QT4fwpMeJf36 ok",
        "bearer [secret] ok",
        &["eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.SflKxwRJSMeKKF2QT4fwpMeJf36"],
    ),
    // --- IBAN (contiguous + space-grouped) --------------------------------------------
    (
        "wire to GB29NWBK60161331926819 by EOD",
        "wire to [iban] by EOD",
        &["GB29NWBK60161331926819", "NWBK"],
    ),
    (
        "IBAN GB29 NWBK 6016 1331 9268 19 thanks",
        "IBAN [iban] thanks",
        &["NWBK", "6016 1331"],
    ),
    // --- URL ---------------------------------------------------------------------------
    (
        "docs at https://internal.example.com/runbook?id=42 here",
        "docs at [url] here",
        &["internal.example.com"],
    ),
    ("see www.example.org/help.", "see [url].", &["example.org"]),
    // --- Email + phone/card (pre-existing categories, locked) --------------------------
    (
        "reach jane.doe@example.com or +1 (555) 123-4567",
        "reach [email] or [redacted-number]",
        &["jane.doe@example.com", "123-4567"],
    ),
    (
        "card 4111 1111 1111 1111 on file",
        "card [redacted-number] on file",
        &["4111 1111 1111 1111"],
    ),
    // --- Mixed / multi-category --------------------------------------------------------
    (
        "key sk-ABCDEFGHIJKLMNOP0123 url https://x.io/a pay GB29NWBK60161331926819 mail a@b.co",
        "key [secret] url [url] pay [iban] mail [email]",
        &[
            "sk-ABCDEFGHIJKLMNOP0123",
            "x.io",
            "GB29NWBK60161331926819",
            "a@b.co",
        ],
    ),
    // --- Negative controls: clean content is untouched --------------------------------
    (
        "Order v1.2 has 3 items in folder_receipts.",
        "Order v1.2 has 3 items in folder_receipts.",
        &[],
    ),
    (
        "Thanks, talk Friday at 9.",
        "Thanks, talk Friday at 9.",
        &[],
    ),
];

#[test]
fn golden_corpus_redacts_exactly_and_idempotently() {
    for (raw, expected, must_not_survive) in GOLDEN {
        let got = redact_text(raw);
        assert_eq!(&got, expected, "exact redaction regressed for {raw:?}");
        for needle in *must_not_survive {
            assert!(
                !got.contains(needle),
                "secret/PII {needle:?} survived redaction of {raw:?} -> {got:?}"
            );
        }
        assert_eq!(
            redact_text(&got),
            got,
            "redaction must be idempotent for {raw:?}"
        );
    }
}

#[test]
fn every_matrix_category_is_exercised_by_the_corpus() {
    let blob: String = GOLDEN
        .iter()
        .map(|(_, exp, _)| *exp)
        .collect::<Vec<_>>()
        .join(" ");
    for placeholder in [
        "[secret]",
        "[iban]",
        "[url]",
        "[email]",
        "[redacted-number]",
    ] {
        assert!(
            blob.contains(placeholder),
            "the golden corpus must cover {placeholder} — coverage gap"
        );
    }
}

#[test]
fn the_secret_floor_holds_at_a_full_ceiling() {
    // `scrub_secrets` is what a Full-ceiling export applies: credentials/IBANs gone, content
    // (URLs, emails, prose) retained.
    let raw = "Hi! key sk-LIVEabcdefGHIJKLMN0123 wire GB29NWBK60161331926819 see https://x.io mail a@b.co";
    let got = scrub_secrets(raw);
    assert!(
        got.contains("[secret]") && got.contains("[iban]"),
        "{got:?}"
    );
    assert!(!got.contains("sk-LIVE") && !got.contains("NWBK"), "{got:?}");
    // Content survives the secret floor (this is a Full export, not a Redacted one).
    assert!(got.contains("https://x.io"), "{got:?}");
    assert!(got.contains("a@b.co"), "{got:?}");
    assert!(got.contains("Hi!"), "{got:?}");
}
