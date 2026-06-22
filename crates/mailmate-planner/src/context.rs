//! The field-environment builders that turn a message (+ its features, + for P2 its
//! classification) into the `RuleEvaluationContext` the deterministic rule engine reads.
//!
//! Pure functions: the same inputs yield the same field map every time, so a learned trait's
//! back-test is replayable. The two pipelines expose different fields — P2 adds the
//! `classification.*` namespace so action rules can key off the P1 verdict.

use std::collections::BTreeMap;

use mailmate_common::classification::Classification;
use mailmate_common::features::{FeatureValue, FeatureVector};
use mailmate_common::ids::DecisionId;
use mailmate_common::mail::MessageData;
use mailmate_common::rules::condition::FieldValue;
use mailmate_common::rules::evaluation::RuleEvaluationContext;

/// Map a deterministic feature value to a rule field value. `Json` features carry no
/// linearly-comparable scalar, so they are not exposed to the condition language.
fn feature_to_field(value: &FeatureValue) -> Option<FieldValue> {
    match value {
        FeatureValue::Bool(b) => Some(FieldValue::Bool(*b)),
        FeatureValue::Number(n) => Some(FieldValue::Float(*n)),
        FeatureValue::Text(s) => Some(FieldValue::Text(s.clone())),
        FeatureValue::Json(_) => None,
    }
}

/// The sender's lowercased domain, parsed from a `From` header (`Name <a@b.com>` → `b.com`).
fn sender_domain(from: &str) -> Option<String> {
    from.rsplit_once('@')
        .map(|(_, domain)| domain.trim_end_matches('>').trim().to_lowercase())
        .filter(|domain| !domain.is_empty())
}

/// The header/feature fields shared by both pipelines. Header-derived fields take
/// precedence over a same-named feature (the extractor cannot shadow `from`/`subject`).
fn base_fields(message: &MessageData, features: &FeatureVector) -> BTreeMap<String, FieldValue> {
    let mut fields = BTreeMap::new();
    for (name, value) in &features.features {
        if let Some(field) = feature_to_field(value) {
            fields.insert(name.clone(), field);
        }
    }
    fields.insert(
        "from".to_owned(),
        FieldValue::Text(message.headers.from.clone()),
    );
    if let Some(domain) = sender_domain(&message.headers.from) {
        fields.insert("sender_domain".to_owned(), FieldValue::Text(domain));
    }
    fields.insert(
        "subject".to_owned(),
        FieldValue::Text(message.headers.subject.clone()),
    );
    fields.insert(
        "folder".to_owned(),
        FieldValue::Text(message.folder_id.as_str().to_owned()),
    );
    fields.insert(
        "has_attachments".to_owned(),
        FieldValue::Bool(!message.attachments.is_empty()),
    );
    fields
}

/// The Pipeline-1 (classification) field environment: headers + deterministic features.
#[must_use]
pub fn classification_context(
    decision_id: DecisionId,
    message: &MessageData,
    features: &FeatureVector,
) -> RuleEvaluationContext {
    RuleEvaluationContext {
        decision_id,
        fields: base_fields(message, features),
    }
}

/// The Pipeline-2 (action) field environment: the base fields plus the `classification.*`
/// namespace, so action rules can key off the P1 verdict.
#[must_use]
pub fn action_context(
    decision_id: DecisionId,
    message: &MessageData,
    classification: &Classification,
    features: &FeatureVector,
) -> RuleEvaluationContext {
    let mut fields = base_fields(message, features);
    fields.insert(
        "classification.labels".to_owned(),
        FieldValue::TextSet(classification.labels.clone()),
    );
    fields.insert(
        "classification.priority".to_owned(),
        FieldValue::Text(classification.priority.as_str().to_owned()),
    );
    fields.insert(
        "classification.spam_score".to_owned(),
        FieldValue::Float(classification.spam_score),
    );
    fields.insert(
        "classification.phishing_score".to_owned(),
        FieldValue::Float(classification.phishing_score),
    );
    fields.insert(
        "classification.needs_review".to_owned(),
        FieldValue::Bool(classification.needs_review),
    );
    RuleEvaluationContext {
        decision_id,
        fields,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::classification::{ClassificationProvenance, Priority};
    use mailmate_common::ids::{AccountId, FolderId, MessageId};
    use mailmate_common::mail::{Attachment, MessageHeaders};

    fn message() -> MessageData {
        MessageData {
            id: Some(MessageId::from("msg_1")),
            client_message_id: "1".to_owned(),
            account_id: AccountId::from("acct_a"),
            folder_id: FolderId::from("folder_inbox"),
            thread_id: None,
            headers: MessageHeaders {
                from: "Vendor Billing <billing@vendor.example>".to_owned(),
                subject: "Invoice #42".to_owned(),
                ..MessageHeaders::default()
            },
            body_text: None,
            attachments: vec![Attachment {
                filename: "x.pdf".to_owned(),
                content_type: "application/pdf".to_owned(),
                size_bytes: 1,
            }],
            remote_content_loaded: false,
            sender_seen_count: None,
            sender_in_address_book: None,
        }
    }

    fn features() -> FeatureVector {
        let mut fv = FeatureVector::new();
        fv.insert("subject_len", FeatureValue::Number(10.0));
        fv.insert(
            "structured",
            FeatureValue::Json(serde_json::json!({"x": 1})),
        );
        fv
    }

    fn classification() -> Classification {
        Classification {
            decision_id: DecisionId::from("dec_1"),
            labels: vec!["invoice".to_owned()],
            spam_score: 0.1,
            phishing_score: 0.0,
            priority: Priority::High,
            needs_review: false,
            confidence: 0.0,
            salient_signals: Vec::new(),
            safety_findings: Vec::new(),
            provenance: ClassificationProvenance::tier1(vec![]),
        }
    }

    #[test]
    fn p1_context_exposes_headers_features_and_parsed_domain() {
        let ctx = classification_context(DecisionId::from("dec_1"), &message(), &features());
        assert_eq!(
            ctx.fields.get("sender_domain"),
            Some(&FieldValue::Text("vendor.example".to_owned()))
        );
        assert_eq!(
            ctx.fields.get("has_attachments"),
            Some(&FieldValue::Bool(true))
        );
        assert_eq!(
            ctx.fields.get("subject_len"),
            Some(&FieldValue::Float(10.0))
        );
        // Json features are not exposed to the condition language.
        assert!(!ctx.fields.contains_key("structured"));
    }

    #[test]
    fn p2_context_adds_the_classification_namespace() {
        let ctx = action_context(
            DecisionId::from("dec_1"),
            &message(),
            &classification(),
            &features(),
        );
        assert_eq!(
            ctx.fields.get("classification.labels"),
            Some(&FieldValue::TextSet(vec!["invoice".to_owned()]))
        );
        assert_eq!(
            ctx.fields.get("classification.priority"),
            Some(&FieldValue::Text("high".to_owned()))
        );
        assert_eq!(
            ctx.fields.get("classification.spam_score"),
            Some(&FieldValue::Float(0.1))
        );
    }

    #[test]
    fn sender_domain_handles_bare_addresses_and_missing_at() {
        assert_eq!(sender_domain("a@b.com"), Some("b.com".to_owned()));
        assert_eq!(sender_domain("no-at-sign"), None);
    }
}
