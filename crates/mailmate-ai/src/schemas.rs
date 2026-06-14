//! Task response schemas: the typed structures the five AI tasks deserialize into, plus
//! the JSON schemas sent to schema-capable providers.
//!
//! Every response type uses `#[serde(deny_unknown_fields)]`, so a provider that invents an
//! extra field is rejected at deserialization — "no invented provider fields leak into core
//! logic" is enforced mechanically, not by review. The [`ValidatedResponse`] trait adds the
//! semantic checks serde cannot express (e.g. a confidence must be in `[0, 1]`).

use serde::{Deserialize, Serialize};

pub use mailmate_common::classification::Priority;

/// A semantic self-check run after a response deserializes — the checks beyond serde's
/// structural ones (required fields, types, enum values, no unknown fields).
pub trait ValidatedResponse {
    /// Validate semantic constraints; `Err(reason)` rejects the response.
    ///
    /// # Errors
    /// A human-readable reason when a constraint (e.g. a confidence range) is violated.
    fn validate(&self) -> Result<(), String>;
}

fn check_unit_interval(name: &str, value: f32) -> Result<(), String> {
    if (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(format!("{name} must be in [0, 1], got {value}"))
    }
}

/// `classify_email` response.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClassifyEmailResponse {
    /// Assigned labels.
    pub labels: Vec<String>,
    /// Spam confidence in `[0, 1]`.
    pub spam_score: f32,
    /// Phishing confidence in `[0, 1]`.
    pub phishing_score: f32,
    /// Assigned priority.
    pub priority: Priority,
}

impl ValidatedResponse for ClassifyEmailResponse {
    fn validate(&self) -> Result<(), String> {
        check_unit_interval("spam_score", self.spam_score)?;
        check_unit_interval("phishing_score", self.phishing_score)
    }
}

/// `draft_reply` response. The draft is advisory text only — it is always review-required
/// downstream and can never itself be sent.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DraftReplyResponse {
    /// Draft subject.
    pub subject: String,
    /// Draft body.
    pub body: String,
    /// Safety notes the review surface should show.
    #[serde(default)]
    pub safety_notes: Vec<String>,
}

impl ValidatedResponse for DraftReplyResponse {
    fn validate(&self) -> Result<(), String> {
        if self.body.is_empty() {
            Err("draft body must not be empty".to_owned())
        } else {
            Ok(())
        }
    }
}

/// `summarize_thread` response.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ThreadSummaryResponse {
    /// The summary text.
    pub summary: String,
    /// Key points.
    #[serde(default)]
    pub key_points: Vec<String>,
}

impl ValidatedResponse for ThreadSummaryResponse {
    fn validate(&self) -> Result<(), String> {
        if self.summary.is_empty() {
            Err("summary must not be empty".to_owned())
        } else {
            Ok(())
        }
    }
}

/// One extracted task.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractedTask {
    /// What needs doing.
    pub description: String,
    /// An optional due date (ISO-8601), if the model found one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
}

/// `extract_tasks` response.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractTasksResponse {
    /// The extracted tasks.
    pub tasks: Vec<ExtractedTask>,
}

impl ValidatedResponse for ExtractTasksResponse {
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

/// One proposed rule (advisory — it enters the rule lifecycle as a candidate, never active).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedRule {
    /// A human title.
    pub title: String,
    /// A description of what the rule would do.
    pub description: String,
}

/// `propose_rules` response.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProposeRulesResponse {
    /// The proposed rules.
    pub proposals: Vec<ProposedRule>,
    /// The model's rationale.
    pub rationale: String,
}

impl ValidatedResponse for ProposeRulesResponse {
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classify_response_rejects_unknown_fields() {
        let with_extra = json!({
            "labels": ["receipt"], "spam_score": 0.1, "phishing_score": 0.0,
            "priority": "normal", "secret_backdoor": true
        });
        let parsed: Result<ClassifyEmailResponse, _> = serde_json::from_value(with_extra);
        assert!(parsed.is_err(), "an invented field must be rejected");
    }

    #[test]
    fn classify_response_validates_confidence_ranges() {
        let bad = ClassifyEmailResponse {
            labels: vec![],
            spam_score: 1.5,
            phishing_score: 0.0,
            priority: Priority::Normal,
        };
        assert!(bad.validate().is_err());
    }
}
