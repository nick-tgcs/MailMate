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
use mailmate_common::mail::MessageData;
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

        // Sender identity (domain only — a one-hot the linear model can weight per domain).
        fv.insert(
            "sender_domain",
            FeatureValue::Text(sender_domain(&headers.from)),
        );

        // Attachment signals.
        fv.insert(
            "has_attachments",
            FeatureValue::Bool(!msg.attachments.is_empty()),
        );
        fv.insert(
            "attachment_count",
            FeatureValue::Number(msg.attachments.len() as f64),
        );

        // Addressing breadth (recipient fan-out is a cheap spam/bulk signal).
        fv.insert(
            "recipient_count",
            FeatureValue::Number(headers.to.len() as f64),
        );

        // Threading: a message that is part of an existing conversation is rarely spam.
        let threaded = headers.in_reply_to.is_some() || !headers.references.is_empty();
        fv.insert("is_threaded", FeatureValue::Bool(threaded));

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
        }
    }

    fn attachment() -> Attachment {
        Attachment {
            filename: "invoice.pdf".to_owned(),
            content_type: "application/pdf".to_owned(),
            size_bytes: 1024,
        }
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
}
