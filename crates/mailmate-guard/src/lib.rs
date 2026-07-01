//! The model-free **commitments guard**.
//!
//! [`scan_commitments`] reads a reply-draft body and returns every commitment it makes, in one
//! of four classes — dates, prices, payment terms, legal/binding language — each cited to the
//! exact span of text that triggered it. There is no model in this path: the patterns are fixed
//! RE2 (linear-time, no ReDoS) regexes, so the same draft always yields the same verdict. That
//! is the determinism-first contract for the trust surface: a draft can never silently commit
//! the user to something the guard didn't name.
//!
//! The guard is *advisory*. It asserts only that the draft **makes** a commitment, never that
//! the commitment is wrong — the human reviewing the draft owns that judgement. The host runs it
//! over every drafted/regenerated body and ships the report to the compose-review panel.

use std::sync::OnceLock;

use regex::Regex;

use mailmate_common::reply::{CommitmentCategory, CommitmentFinding, CommitmentGuardReport};

/// Scan a reply-draft `body` for the four classes of commitment, returning the findings in body
/// order (ascending start), each cited to its char-offset span.
///
/// Findings fully contained within a longer finding of the same category are dropped, so a
/// single phrase is reported once at its widest span.
#[must_use]
pub fn scan_commitments(body: &str) -> CommitmentGuardReport {
    let mut findings = Vec::new();
    for (category, regex) in patterns() {
        for m in regex.find_iter(body) {
            findings.push(CommitmentFinding {
                category: *category,
                text: body[m.start()..m.end()].to_owned(),
                start: char_offset(body, m.start()),
                end: char_offset(body, m.end()),
            });
        }
    }
    CommitmentGuardReport {
        findings: dedup_contained(findings),
    }
}

/// The number of `char`s in `body` before byte index `byte` (which the regex guarantees lands on
/// a char boundary). Char offsets — not byte offsets — are cited so the renderer can highlight
/// the span without tripping over multi-byte text.
fn char_offset(body: &str, byte: usize) -> usize {
    body[..byte].chars().count()
}

/// Drop any finding whose span is fully contained in a same-category finding, keeping the wider
/// one; return the survivors in body order.
fn dedup_contained(mut findings: Vec<CommitmentFinding>) -> Vec<CommitmentFinding> {
    // Widest-first at each start, so a containing span is always considered before the spans it
    // swallows.
    findings.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut kept: Vec<CommitmentFinding> = Vec::new();
    for finding in findings {
        let swallowed = kept.iter().any(|k| {
            k.category == finding.category && k.start <= finding.start && finding.end <= k.end
        });
        if !swallowed {
            kept.push(finding);
        }
    }
    kept.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
    kept
}

/// The compiled category patterns, built once. Each pattern is case-insensitive and anchored on
/// word boundaries to keep false positives down; the set is deliberately conservative (it names
/// the obvious commitment phrasings, not every conceivable one) because a missed flag is a
/// softer failure than a guard that cries wolf on ordinary prose.
fn patterns() -> &'static [(CommitmentCategory, Regex)] {
    static PATTERNS: OnceLock<Vec<(CommitmentCategory, Regex)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let compile = |src: &str| Regex::new(src).expect("guard pattern is a valid regex");
        vec![
            (
                CommitmentCategory::Date,
                compile(concat!(
                    r"(?i)",
                    r"\b(?:monday|tuesday|wednesday|thursday|friday|saturday|sunday)\b",
                    r"|\b(?:today|tonight|tomorrow|yesterday)\b",
                    r"|\b(?:next|this|by|before)\s+(?:week|month|quarter|year)\b",
                    r"|\bend\s+of\s+(?:the\s+)?(?:week|month|day|quarter)\b",
                    r"|\b(?:jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|jun(?:e)?|jul(?:y)?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)\.?\s+\d{1,2}(?:st|nd|rd|th)?\b",
                    r"|\b\d{1,2}[/-]\d{1,2}(?:[/-]\d{2,4})?\b",
                    r"|\b\d{4}-\d{2}-\d{2}\b",
                    r"|\b(?:eod|eow|cob)\b",
                )),
            ),
            (
                CommitmentCategory::Price,
                compile(concat!(
                    r"(?i)",
                    r"[$£€¥]\s?\d[\d,]*(?:\.\d{1,2})?",
                    r"|\b\d[\d,]*(?:\.\d{1,2})?\s?(?:usd|eur|gbp|aud|cad|dollars?|euros?|pounds?)\b",
                    r"|\b\d{1,3}\s?%(?:\s*(?:off|discount|rebate))?",
                )),
            ),
            (
                CommitmentCategory::Payment,
                compile(concat!(
                    r"(?i)",
                    r"\bnet\s?\d{1,3}\b",
                    r"|\binvoices?\b|\bdeposits?\b|\brefunds?\b",
                    r"|\b(?:wire|bank)\s+transfers?\b",
                    r"|\bpayment\s+terms?\b",
                    r"|\bpay(?:able|ment)?\s+(?:within|in)\s+\d+\s+days?\b",
                    r"|\binstall?ments?\b|\bdown\s+payments?\b|\bpre-?payments?\b|\bup-?front\b",
                    r"|\bpurchase\s+orders?\b",
                )),
            ),
            (
                CommitmentCategory::Legal,
                compile(concat!(
                    r"(?i)",
                    r"\b(?:i|we)\s+(?:agree|confirm|commit|promise|accept)\b",
                    r"|\bguarantee[ds]?\b|\bwarrant(?:s|ed|y|ies)?\b",
                    r"|\b(?:legally\s+)?binding\b",
                    r"|\bcontracts?\b|\bagreements?\b",
                    r"|\bliabilit(?:y|ies)\b|\bliable\b",
                    r"|\bindemnif(?:y|ies|ied|ication)\b",
                    r"|\bterms\s+and\s+conditions\b",
                )),
            ),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn categories(report: &CommitmentGuardReport) -> Vec<CommitmentCategory> {
        report.categories()
    }

    #[test]
    fn a_benign_acknowledgement_is_all_clear() {
        let report = scan_commitments(
            "Hi,\n\nThanks for reaching out. I'll take a look and get back to you.\n\nBest,\nN",
        );
        assert!(
            report.is_clear(),
            "unexpected findings: {:?}",
            report.findings
        );
    }

    #[test]
    fn a_weekday_deadline_is_flagged_as_a_date_with_its_span() {
        let body = "I can have it to you by Friday.";
        let report = scan_commitments(body);
        assert_eq!(report.count(), 1);
        let finding = &report.findings[0];
        assert_eq!(finding.category, CommitmentCategory::Date);
        assert_eq!(finding.text, "Friday");
        // The cited span re-slices to exactly the matched text.
        let chars: Vec<char> = body.chars().collect();
        let cited: String = chars[finding.start..finding.end].iter().collect();
        assert_eq!(cited, "Friday");
    }

    #[test]
    fn numeric_and_iso_dates_are_flagged() {
        assert_eq!(
            scan_commitments("Let's meet 3/14 to confirm.").findings[0].text,
            "3/14"
        );
        assert_eq!(
            scan_commitments("Target date is 2026-06-22.").findings[0].text,
            "2026-06-22"
        );
        let month = scan_commitments("I'll send it on March 3rd.");
        assert_eq!(month.findings[0].category, CommitmentCategory::Date);
        assert_eq!(month.findings[0].text, "March 3rd");
    }

    #[test]
    fn a_currency_amount_is_flagged_as_a_price() {
        let report = scan_commitments("We can do it for $1,200 total.");
        assert_eq!(report.count(), 1);
        assert_eq!(report.findings[0].category, CommitmentCategory::Price);
        assert_eq!(report.findings[0].text, "$1,200");
    }

    #[test]
    fn a_currency_code_and_a_percentage_are_prices() {
        assert_eq!(
            scan_commitments("That's 950 USD.").findings[0].text,
            "950 USD"
        );
        let pct = scan_commitments("I can offer 15% off.");
        assert_eq!(pct.findings[0].category, CommitmentCategory::Price);
        assert!(pct.findings[0].text.starts_with("15%"));
    }

    #[test]
    fn payment_terms_are_flagged() {
        let report = scan_commitments("Please send the invoice; our terms are net 30.");
        let cats = categories(&report);
        assert!(cats.contains(&CommitmentCategory::Payment), "got {cats:?}");
        let texts: Vec<&str> = report.findings.iter().map(|f| f.text.as_str()).collect();
        assert!(
            texts.iter().any(|t| t.eq_ignore_ascii_case("invoice")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.to_lowercase().starts_with("net 30")),
            "{texts:?}"
        );
    }

    #[test]
    fn binding_language_is_flagged_as_legal() {
        let report = scan_commitments("Yes, I agree to the terms and we guarantee delivery.");
        let cats = categories(&report);
        assert_eq!(cats, vec![CommitmentCategory::Legal]);
        let texts: Vec<String> = report
            .findings
            .iter()
            .map(|f| f.text.to_lowercase())
            .collect();
        assert!(texts.contains(&"i agree".to_owned()), "{texts:?}");
        assert!(texts.contains(&"guarantee".to_owned()), "{texts:?}");
    }

    #[test]
    fn findings_come_back_in_body_order_across_categories() {
        let body = "By Monday I'll wire the deposit of $500 and I agree to the contract.";
        let report = scan_commitments(body);
        let starts: Vec<usize> = report.findings.iter().map(|f| f.start).collect();
        let mut sorted = starts.clone();
        sorted.sort_unstable();
        assert_eq!(starts, sorted, "findings must be in ascending body order");
        // All four categories appear in this one sentence.
        assert_eq!(
            report.categories(),
            vec![
                CommitmentCategory::Date,
                CommitmentCategory::Price,
                CommitmentCategory::Payment,
                CommitmentCategory::Legal
            ]
        );
    }

    #[test]
    fn spans_are_char_offsets_not_byte_offsets() {
        // A leading multi-byte char shifts byte offsets past char offsets; the cited span must
        // still re-slice (by char) to the matched text.
        let body = "café — due Friday";
        let report = scan_commitments(body);
        assert_eq!(report.count(), 1);
        let f = &report.findings[0];
        let chars: Vec<char> = body.chars().collect();
        let cited: String = chars[f.start..f.end].iter().collect();
        assert_eq!(cited, "Friday");
    }

    #[test]
    fn overlapping_same_category_matches_are_reported_once() {
        // "purchase order" must not also surface a nested shorter payment match.
        let report = scan_commitments("Attached is the purchase order.");
        let payments: Vec<&CommitmentFinding> = report
            .findings
            .iter()
            .filter(|f| f.category == CommitmentCategory::Payment)
            .collect();
        assert_eq!(payments.len(), 1, "got {payments:?}");
        assert_eq!(payments[0].text.to_lowercase(), "purchase order");
    }
}
