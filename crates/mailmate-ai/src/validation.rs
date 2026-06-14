//! Structured-response validation: deserialize a provider's structured response into a
//! typed task result, enforcing schema compliance, required fields, enum values, no
//! invented fields (via `deny_unknown_fields`), and the semantic
//! [`ValidatedResponse`] checks.
//!
//! A response that fails here is a **validation failure**: the caller records it as an
//! audit event and it never drives an action. (Schema-*valid* responses can still be
//! rejected by a *later* layer — the policy guard — which is a separate concern.)

use serde::de::DeserializeOwned;

use mailmate_common::ai::StructuredResponse;
use mailmate_common::error::AiError;

use crate::schemas::ValidatedResponse;

/// Parse and validate a provider response into the typed task result `T`.
///
/// # Errors
/// [`AiError::Validation`] if the JSON does not match `T`'s schema (missing/extra fields,
/// wrong type, bad enum) or fails `T`'s semantic [`ValidatedResponse::validate`].
pub fn validate_and_parse<T: DeserializeOwned + ValidatedResponse>(
    response: &StructuredResponse,
) -> Result<T, AiError> {
    let parsed: T = serde_json::from_value(response.parsed_json.clone())
        .map_err(|e| AiError::Validation(e.to_string()))?;
    parsed.validate().map_err(AiError::Validation)?;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::schemas::{ClassifyEmailResponse, Priority};

    fn response(value: serde_json::Value) -> StructuredResponse {
        StructuredResponse {
            raw_text: value.to_string(),
            parsed_json: value,
            schema_validated_by: Some("mock".to_owned()),
        }
    }

    #[test]
    fn parses_a_valid_response() {
        let resp = response(json!({
            "labels": ["receipt"], "spam_score": 0.05, "phishing_score": 0.0, "priority": "low"
        }));
        let parsed: ClassifyEmailResponse = validate_and_parse(&resp).unwrap();
        assert_eq!(parsed.priority, Priority::Low);
    }

    #[test]
    fn rejects_a_schema_mismatch() {
        let resp = response(json!({ "totally": "wrong" }));
        let err = validate_and_parse::<ClassifyEmailResponse>(&resp).unwrap_err();
        assert!(matches!(err, AiError::Validation(_)), "got {err:?}");
    }

    #[test]
    fn rejects_a_bad_enum_value() {
        let resp = response(json!({
            "labels": [], "spam_score": 0.0, "phishing_score": 0.0, "priority": "supercritical"
        }));
        assert!(validate_and_parse::<ClassifyEmailResponse>(&resp).is_err());
    }
}
