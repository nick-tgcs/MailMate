//! The production [`FeatureExtractor`]: a pure, deterministic message → feature-vector
//! function over **non-body** signals only.
//!
//! This is the real adapter behind the `FeatureExtractor` port that the cascade's Tier-2
//! model consumes and crystallization back-tests replay. It MUST be pure (same
//! [`MessageData`] → identical [`FeatureVector`], no I/O) — that purity is the contract that
//! makes a learned trait's back-test reproducible. It lives beside [`LogisticRegressionClassifier`]
//! (`crate::logreg`) because the extractor and the model that consumes its output are one
//! featurization story; the inner hexagon sees only the port.
//!
//! Every feature is derived from headers/attachment metadata — never the body, never the
//! network (`from` is parsed locally; no domain is resolved). The set is intentionally small
//! and stable; richer features are added by appending keys, never by changing an existing
//! key's meaning (a back-test pins on key names).

use mailmate_common::features::{FeatureValue, FeatureVector};
use mailmate_common::mail::{Attachment, MessageData};
use mailmate_ports::feature_extractor::FeatureExtractor;

/// The default, deterministic non-body feature extractor.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicFeatureExtractor;

impl DeterministicFeatureExtractor {
    /// Construct the extractor.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// The lowercased domain part of an email address (`a@b.com` → `b.com`), or `""` when there
/// is no `@`. Tolerates a `Display Name <addr>` form by taking the bracketed address. Pure
/// string work — no DNS, no network.
fn sender_domain(from: &str) -> String {
    let addr = match (from.find('<'), from.rfind('>')) {
        (Some(l), Some(r)) if r > l + 1 => &from[l + 1..r],
        _ => from,
    };
    addr.rsplit_once('@')
        .map_or_else(String::new, |(_, domain)| {
            domain.trim().to_ascii_lowercase()
        })
}

/// Whether a subject is a reply/forward (case-insensitive `re:` / `fw:` / `fwd:` prefix).
fn is_reply_or_forward(subject: &str) -> bool {
    let s = subject.trim_start().to_ascii_lowercase();
    s.starts_with("re:") || s.starts_with("fw:") || s.starts_with("fwd:")
}

/// Split a `From`-style value into `(display_name, address)`, both lowercased. Tolerates a
/// bare address (no display name) and a quoted display name. Pure string work.
fn parse_from(from: &str) -> (String, String) {
    match (from.find('<'), from.rfind('>')) {
        (Some(l), Some(r)) if r > l + 1 => {
            let name = from[..l].trim().trim_matches('"').to_ascii_lowercase();
            let addr = from[l + 1..r].trim().to_ascii_lowercase();
            (name, addr)
        }
        _ => (String::new(), from.trim().to_ascii_lowercase()),
    }
}

/// The lowercased token an `Authentication-Results` value records for `method` (e.g. the
/// `pass` in `spf=pass`), or `None` if the method is absent. The haystack must already be
/// lowercased. No network — this only reads the verdict the receiving MTA already wrote.
fn auth_token(results_lower: &str, method: &str) -> Option<String> {
    let needle = format!("{method}=");
    let start = results_lower.find(&needle)? + needle.len();
    let token: String = results_lower[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != ';' && *c != '(')
        .collect();
    (!token.is_empty()).then_some(token)
}

/// Whether any `From`-domain-mismatching email address is embedded in the display name — a
/// common spoofing trick (`"billing@trusted.test" <attacker@evil.test>`).
fn display_name_spoofed(display_name: &str, from_domain: &str) -> bool {
    display_name.split_whitespace().any(|tok| {
        tok.contains('@') && {
            let d = address_domain(tok);
            !d.is_empty() && d != from_domain
        }
    })
}

/// The lowercased domain of a bare address (`a@b.com` → `b.com`), or `""`.
fn address_domain(addr: &str) -> String {
    addr.rsplit_once('@').map_or_else(String::new, |(_, d)| {
        d.trim().trim_end_matches('>').to_ascii_lowercase()
    })
}

/// Whether `filename` (case-insensitively) ends with any of `exts` (each given without a dot).
fn has_ext(filename: &str, exts: &[&str]) -> bool {
    let lower = filename.to_ascii_lowercase();
    exts.iter().any(|e| lower.ends_with(&format!(".{e}")))
}

/// Whether an attachment looks like an invoice/receipt PDF (a finance/commitment cue).
fn is_invoice_pdf(att: &Attachment) -> bool {
    let name = att.filename.to_ascii_lowercase();
    let is_pdf =
        att.content_type.eq_ignore_ascii_case("application/pdf") || has_ext(&name, &["pdf"]);
    is_pdf
        && ["invoice", "receipt", "statement", "bill"]
            .iter()
            .any(|k| name.contains(k))
}

/// Whether an attachment is a calendar invite.
fn is_calendar(att: &Attachment) -> bool {
    att.content_type.to_ascii_lowercase().contains("calendar") || has_ext(&att.filename, &["ics"])
}

/// Whether an attachment is a runnable executable/script (a dangerous-attachment cue).
fn is_executable(att: &Attachment) -> bool {
    let ct = att.content_type.to_ascii_lowercase();
    ct == "application/x-msdownload"
        || ct == "application/x-executable"
        || ct == "application/x-msdos-program"
        || has_ext(
            &att.filename,
            &[
                "exe", "scr", "bat", "cmd", "com", "js", "vbs", "jar", "msi", "ps1", "lnk",
            ],
        )
}

/// Whether an attachment is an archive (a malware-delivery cue when combined with the above).
fn is_archive(att: &Attachment) -> bool {
    let ct = att.content_type.to_ascii_lowercase();
    ct.contains("zip")
        || ct.contains("rar")
        || ct.contains("7z")
        || has_ext(&att.filename, &["zip", "rar", "7z", "tar", "gz", "tgz"])
}

/// Whether a subject carries an urgency/pressure cue (a phishing/scam social-engineering tell).
fn subject_has_urgency(subject_lower: &str) -> bool {
    [
        "urgent",
        "immediately",
        "asap",
        "act now",
        "verify your",
        "suspended",
        "account locked",
        "final notice",
        "past due",
        "action required",
    ]
    .iter()
    .any(|cue| subject_lower.contains(cue))
}

impl FeatureExtractor for DeterministicFeatureExtractor {
    fn extract(&self, msg: &MessageData) -> FeatureVector {
        let headers = &msg.headers;
        let mut fv = FeatureVector::new();

        // Subject shape (length in chars, not bytes, so multi-byte subjects are stable).
        fv.insert(
            "subject_len",
            FeatureValue::Number(headers.subject.chars().count() as f64),
        );
        fv.insert(
            "subject_is_reply",
            FeatureValue::Bool(is_reply_or_forward(&headers.subject)),
        );

        // Sender identity. The domain is the one-hot the linear model weights per domain; the
        // full address and display name are kept so richer rules can key on them too.
        let (display_name, address) = parse_from(&headers.from);
        let from_domain = sender_domain(&headers.from);
        fv.insert("sender_domain", FeatureValue::Text(from_domain.clone()));
        fv.insert("sender_address", FeatureValue::Text(address));
        fv.insert(
            "sender_display_name",
            FeatureValue::Text(display_name.clone()),
        );

        // Sender relationship (client-derived context; absent ⇒ the conservative default).
        fv.insert(
            "sender_seen_count",
            FeatureValue::Number(f64::from(msg.sender_seen_count.unwrap_or(0))),
        );
        fv.insert(
            "in_address_book",
            FeatureValue::Bool(msg.sender_in_address_book.unwrap_or(false)),
        );

        // Attachment signals — count plus the security/finance-relevant classes.
        fv.insert(
            "has_attachments",
            FeatureValue::Bool(!msg.attachments.is_empty()),
        );
        fv.insert(
            "attachment_count",
            FeatureValue::Number(msg.attachments.len() as f64),
        );
        fv.insert(
            "has_invoice_pdf",
            FeatureValue::Bool(msg.attachments.iter().any(is_invoice_pdf)),
        );
        fv.insert(
            "has_calendar",
            FeatureValue::Bool(msg.attachments.iter().any(is_calendar)),
        );
        fv.insert(
            "has_executable",
            FeatureValue::Bool(msg.attachments.iter().any(is_executable)),
        );
        fv.insert(
            "has_archive",
            FeatureValue::Bool(msg.attachments.iter().any(is_archive)),
        );

        // Addressing breadth (recipient fan-out is a cheap spam/bulk signal).
        fv.insert(
            "recipient_count",
            FeatureValue::Number(headers.to.len() as f64),
        );

        // Threading: a message that is part of an existing conversation is rarely spam.
        let threaded = headers.in_reply_to.is_some() || !headers.references.is_empty();
        fv.insert("is_threaded", FeatureValue::Bool(threaded));

        // List / bulk mail (newsletters, mailing lists) — the unsubscribe + bulk cues.
        fv.insert(
            "is_list_mail",
            FeatureValue::Bool(headers.list_id.is_some()),
        );
        fv.insert(
            "has_unsubscribe",
            FeatureValue::Bool(headers.list_unsubscribe.is_some()),
        );
        let bulk = headers.precedence.as_deref().is_some_and(|p| {
            let p = p.trim().to_ascii_lowercase();
            p == "bulk" || p == "list" || p == "junk"
        });
        fv.insert("is_bulk", FeatureValue::Bool(bulk));

        // Authentication results (SPF/DKIM/DMARC) — the verdict the receiving MTA already
        // wrote, parsed locally. `auth_fail` is the phishing-relevant aggregate.
        let auth = headers
            .authentication_results
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase();
        fv.insert("auth_present", FeatureValue::Bool(!auth.is_empty()));
        let mut any_fail = false;
        for method in ["spf", "dkim", "dmarc"] {
            let token = auth_token(&auth, method);
            let pass = token.as_deref() == Some("pass");
            let fail = matches!(token.as_deref(), Some("fail" | "softfail" | "permerror"));
            any_fail |= fail;
            fv.insert(format!("{method}_pass"), FeatureValue::Bool(pass));
        }
        fv.insert("auth_fail", FeatureValue::Bool(any_fail));

        // Spoofing cues: a `Reply-To` pointing at a different domain than `From`, and a display
        // name that embeds a mismatching email address.
        let reply_to_mismatch = headers.reply_to.as_deref().is_some_and(|rt| {
            let d = sender_domain(rt);
            !d.is_empty() && !from_domain.is_empty() && d != from_domain
        });
        fv.insert("reply_to_mismatch", FeatureValue::Bool(reply_to_mismatch));
        fv.insert(
            "display_name_spoofed",
            FeatureValue::Bool(display_name_spoofed(&display_name, &from_domain)),
        );

        // Subject social-engineering cues.
        let subject_lower = headers.subject.to_ascii_lowercase();
        fv.insert(
            "subject_has_urgency",
            FeatureValue::Bool(subject_has_urgency(&subject_lower)),
        );

        fv
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::ids::{AccountId, FolderId};
    use mailmate_common::mail::{Attachment, MessageHeaders};

    fn message(headers: MessageHeaders, attachments: Vec<Attachment>) -> MessageData {
        MessageData {
            id: None,
            client_message_id: "1".to_owned(),
            account_id: AccountId::from("acct"),
            folder_id: FolderId::from("inbox"),
            thread_id: None,
            headers,
            body_text: Some("a body that must NEVER influence features".to_owned()),
            attachments,
            remote_content_loaded: false,
            sender_seen_count: None,
            sender_in_address_book: None,
        }
    }

    fn att(filename: &str, content_type: &str) -> Attachment {
        Attachment {
            filename: filename.to_owned(),
            content_type: content_type.to_owned(),
            size_bytes: 1024,
        }
    }

    fn attachment() -> Attachment {
        att("invoice.pdf", "application/pdf")
    }

    #[test]
    fn extracts_the_documented_non_body_feature_set() {
        let headers = MessageHeaders {
            from: "Sales <rep@Acme.TEST>".to_owned(),
            to: vec!["a@x.test".to_owned(), "b@x.test".to_owned()],
            subject: "RE: your quote".to_owned(),
            in_reply_to: Some("<prev@x.test>".to_owned()),
            ..MessageHeaders::default()
        };
        let fv =
            DeterministicFeatureExtractor::new().extract(&message(headers, vec![attachment()]));

        assert_eq!(fv.get("subject_len"), Some(&FeatureValue::Number(14.0)));
        assert_eq!(fv.get("subject_is_reply"), Some(&FeatureValue::Bool(true)));
        // The angle-bracketed display name is tolerated; the domain is lowercased.
        assert_eq!(
            fv.get("sender_domain"),
            Some(&FeatureValue::Text("acme.test".to_owned()))
        );
        assert_eq!(fv.get("has_attachments"), Some(&FeatureValue::Bool(true)));
        assert_eq!(fv.get("attachment_count"), Some(&FeatureValue::Number(1.0)));
        assert_eq!(fv.get("recipient_count"), Some(&FeatureValue::Number(2.0)));
        assert_eq!(fv.get("is_threaded"), Some(&FeatureValue::Bool(true)));
    }

    #[test]
    fn is_pure_and_independent_of_the_body() {
        let headers = MessageHeaders {
            from: "x@y.test".to_owned(),
            subject: "hello".to_owned(),
            ..MessageHeaders::default()
        };
        let mut a = message(headers.clone(), vec![]);
        let extractor = DeterministicFeatureExtractor::new();
        let first = extractor.extract(&a);
        // Mutating the body changes nothing; extracting twice is identical.
        a.body_text = Some("totally different body".to_owned());
        let second = extractor.extract(&a);
        assert_eq!(
            first, second,
            "features are a pure function of non-body data"
        );
        assert_eq!(
            first.get("subject_is_reply"),
            Some(&FeatureValue::Bool(false))
        );
        assert_eq!(first.get("is_threaded"), Some(&FeatureValue::Bool(false)));
        assert_eq!(
            first.get("sender_domain"),
            Some(&FeatureValue::Text("y.test".to_owned()))
        );
    }

    #[test]
    fn a_from_without_an_at_yields_an_empty_domain() {
        let headers = MessageHeaders {
            from: "mailer-daemon".to_owned(),
            ..MessageHeaders::default()
        };
        let fv = DeterministicFeatureExtractor::new().extract(&message(headers, vec![]));
        assert_eq!(
            fv.get("sender_domain"),
            Some(&FeatureValue::Text(String::new()))
        );
    }

    #[test]
    fn parses_authentication_results_into_spf_dkim_dmarc() {
        let headers = MessageHeaders {
            from: "ceo@corp.test".to_owned(),
            authentication_results: Some(
                "mx.google.com; spf=pass smtp.mailfrom=corp.test; dkim=fail header.i=@corp.test; \
                 dmarc=fail (p=REJECT)"
                    .to_owned(),
            ),
            ..MessageHeaders::default()
        };
        let fv = DeterministicFeatureExtractor::new().extract(&message(headers, vec![]));
        assert_eq!(fv.get("auth_present"), Some(&FeatureValue::Bool(true)));
        assert_eq!(fv.get("spf_pass"), Some(&FeatureValue::Bool(true)));
        assert_eq!(fv.get("dkim_pass"), Some(&FeatureValue::Bool(false)));
        assert_eq!(fv.get("dmarc_pass"), Some(&FeatureValue::Bool(false)));
        // A DKIM/DMARC fail trips the phishing-relevant aggregate.
        assert_eq!(fv.get("auth_fail"), Some(&FeatureValue::Bool(true)));
    }

    #[test]
    fn absent_authentication_results_are_not_a_failure() {
        let fv = DeterministicFeatureExtractor::new()
            .extract(&message(MessageHeaders::default(), vec![]));
        assert_eq!(fv.get("auth_present"), Some(&FeatureValue::Bool(false)));
        assert_eq!(fv.get("auth_fail"), Some(&FeatureValue::Bool(false)));
        assert_eq!(fv.get("spf_pass"), Some(&FeatureValue::Bool(false)));
    }

    #[test]
    fn classifies_attachment_security_and_finance_signals() {
        let atts = vec![
            att("Invoice_4821.PDF", "application/pdf"),
            att("setup.exe", "application/octet-stream"),
            att("photos.zip", "application/zip"),
            att("meeting.ics", "text/calendar"),
        ];
        let fv =
            DeterministicFeatureExtractor::new().extract(&message(MessageHeaders::default(), atts));
        assert_eq!(fv.get("has_invoice_pdf"), Some(&FeatureValue::Bool(true)));
        assert_eq!(fv.get("has_executable"), Some(&FeatureValue::Bool(true)));
        assert_eq!(fv.get("has_archive"), Some(&FeatureValue::Bool(true)));
        assert_eq!(fv.get("has_calendar"), Some(&FeatureValue::Bool(true)));
        assert_eq!(fv.get("attachment_count"), Some(&FeatureValue::Number(4.0)));
    }

    #[test]
    fn flags_reply_to_and_display_name_spoofing() {
        let headers = MessageHeaders {
            from: "\"support@paypal.test\" <attacker@evil.test>".to_owned(),
            reply_to: Some("collect@evil.test".to_owned()),
            ..MessageHeaders::default()
        };
        let fv = DeterministicFeatureExtractor::new().extract(&message(headers, vec![]));
        // From domain is the real bracketed address; the display name embeds a different domain.
        assert_eq!(
            fv.get("sender_domain"),
            Some(&FeatureValue::Text("evil.test".to_owned()))
        );
        assert_eq!(
            fv.get("display_name_spoofed"),
            Some(&FeatureValue::Bool(true))
        );
        // Reply-To points at the same evil domain here, so no *mismatch* against From.
        assert_eq!(
            fv.get("reply_to_mismatch"),
            Some(&FeatureValue::Bool(false))
        );

        // A Reply-To on a different domain than From IS a mismatch.
        let headers2 = MessageHeaders {
            from: "billing@trusted.test".to_owned(),
            reply_to: Some("collector@elsewhere.test".to_owned()),
            ..MessageHeaders::default()
        };
        let fv2 = DeterministicFeatureExtractor::new().extract(&message(headers2, vec![]));
        assert_eq!(
            fv2.get("reply_to_mismatch"),
            Some(&FeatureValue::Bool(true))
        );
        assert_eq!(
            fv2.get("display_name_spoofed"),
            Some(&FeatureValue::Bool(false))
        );
    }

    #[test]
    fn marks_list_bulk_and_unsubscribe() {
        let headers = MessageHeaders {
            from: "news@list.test".to_owned(),
            list_id: Some("<newsletter.list.test>".to_owned()),
            list_unsubscribe: Some("<mailto:unsub@list.test>".to_owned()),
            precedence: Some("bulk".to_owned()),
            ..MessageHeaders::default()
        };
        let fv = DeterministicFeatureExtractor::new().extract(&message(headers, vec![]));
        assert_eq!(fv.get("is_list_mail"), Some(&FeatureValue::Bool(true)));
        assert_eq!(fv.get("has_unsubscribe"), Some(&FeatureValue::Bool(true)));
        assert_eq!(fv.get("is_bulk"), Some(&FeatureValue::Bool(true)));
    }

    #[test]
    fn surfaces_sender_relationship_and_urgency() {
        let headers = MessageHeaders {
            from: "Jane Doe <jane@known.test>".to_owned(),
            subject: "URGENT: verify your account immediately".to_owned(),
            ..MessageHeaders::default()
        };
        let mut msg = message(headers, vec![]);
        msg.sender_seen_count = Some(42);
        msg.sender_in_address_book = Some(true);
        let fv = DeterministicFeatureExtractor::new().extract(&msg);
        assert_eq!(
            fv.get("sender_seen_count"),
            Some(&FeatureValue::Number(42.0))
        );
        assert_eq!(fv.get("in_address_book"), Some(&FeatureValue::Bool(true)));
        assert_eq!(
            fv.get("sender_display_name"),
            Some(&FeatureValue::Text("jane doe".to_owned()))
        );
        assert_eq!(
            fv.get("subject_has_urgency"),
            Some(&FeatureValue::Bool(true))
        );
    }
}
