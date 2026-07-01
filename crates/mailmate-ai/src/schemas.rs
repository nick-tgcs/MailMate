//! Task response schemas: the typed structures the five AI tasks deserialize into, plus
//! the JSON schemas sent to schema-capable providers.
//!
//! Every response type uses `#[serde(deny_unknown_fields)]`, so a provider that invents an
//! extra field is rejected at deserialization — "no invented provider fields leak into core
//! logic" is enforced mechanically, not by review. The [`ValidatedResponse`] trait adds the
//! semantic checks serde cannot express (e.g. a confidence must be in `[0, 1]`).

use serde::{Deserialize, Serialize};

pub use mailmate_common::classification::Priority;
use mailmate_common::evidence::EvidenceSourceKind;
use mailmate_common::ids::RuleId;
use mailmate_common::proposal::ProposalKind;
use mailmate_common::rules::rule::{RiskLevel, RuleDraft, RuleKind, RuleStatus};

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
    /// The model's short "why this draft" explanation, when it gave one. Optional on the wire
    /// (`#[serde(default)]`) so an older or terser model that omits it still validates — the
    /// review surface degrades to a generic line rather than inventing a reason.
    #[serde(default)]
    pub rationale: String,
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

/// One curator-proposed change. It reuses the domain proposal/rule vocabulary, so the model
/// must emit a deterministic JSON-AST [`RuleDraft`] (condition + effect) — no free-text rule
/// — and a `recommended_status` the curator is *allowed* to recommend (never `active`).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CuratorProposalDto {
    /// What the proposal asks for (`new_rule`, `refine_rule`, …).
    pub proposal_type: ProposalKind,
    /// The estimated risk of the recommended rule.
    pub risk_level: RiskLevel,
    /// A short human title.
    pub title: String,
    /// A redacted justification.
    pub rationale: String,
    /// The status the curator recommends the rule enter on acceptance. Validated to never be
    /// `active` (the curator may not activate).
    pub recommended_status: RuleStatus,
    /// The candidate rule (required for `new_rule`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_draft: Option<RuleDraft>,
    /// The kind of the existing rule a refine/merge/split/retire targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_rule_kind: Option<RuleKind>,
    /// The existing rule a refine/merge/split/retire targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_rule_id: Option<RuleId>,
}

impl CuratorProposalDto {
    /// Whether this proposal creates a brand-new rule (and so must carry a draft).
    #[must_use]
    pub fn is_new_rule(&self) -> bool {
        self.proposal_type == ProposalKind::NewRule
    }
}

/// One curator suggestion to change an evidence threshold (advisory; never auto-applied).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CuratorThresholdDto {
    /// Which rule kind the threshold gates.
    pub rule_kind: RuleKind,
    /// Which feedback source the threshold counts.
    pub source_kind: EvidenceSourceKind,
    /// The value the curator suggests.
    pub suggested: usize,
    /// Why.
    pub rationale: String,
}

/// `curate_rules` response — the curator's proposals plus advisory observations.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CurateRulesResponse {
    /// The proposed rule changes.
    #[serde(default)]
    pub proposals: Vec<CuratorProposalDto>,
    /// Advisory threshold suggestions.
    #[serde(default)]
    pub threshold_suggestions: Vec<CuratorThresholdDto>,
    /// An advisory, redacted summary of the recent feedback pattern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback_summary: Option<String>,
    /// The curator's overall rationale.
    pub rationale: String,
}

impl ValidatedResponse for CurateRulesResponse {
    fn validate(&self) -> Result<(), String> {
        for proposal in &self.proposals {
            // The curator may PROPOSE but never ACTIVATE: an `active` recommendation is a
            // structural-validation failure that is audited and drives no proposal.
            if proposal.recommended_status == RuleStatus::Active {
                return Err(format!(
                    "curator may not recommend an active rule (proposal {:?})",
                    proposal.title
                ));
            }
            if proposal.is_new_rule() {
                if proposal.rule_draft.is_none() {
                    return Err(format!(
                        "new_rule proposal {:?} must carry a rule_draft",
                        proposal.title
                    ));
                }
            } else if proposal.target_rule_id.is_none() {
                return Err(format!(
                    "{} proposal {:?} must target an existing rule",
                    proposal.proposal_type.as_str(),
                    proposal.title
                ));
            }
        }
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

    fn new_rule_proposal_json() -> serde_json::Value {
        json!({
            "proposal_type": "new_rule",
            "risk_level": "low",
            "title": "File stripe.com to Receipts",
            "rationale": "6 moves from stripe.com to Receipts.",
            "recommended_status": "shadow_mode",
            "rule_draft": {
                "kind": "action",
                "scope": "domain",
                "condition": { "field": "sender_domain", "op": "eq", "value": "stripe.com" },
                "effect": { "move": "Receipts" }
            }
        })
    }

    #[test]
    fn curate_response_parses_a_well_formed_new_rule_proposal() {
        let value = json!({
            "proposals": [new_rule_proposal_json()],
            "threshold_suggestions": [],
            "rationale": "Clustered the stripe receipts."
        });
        let parsed: CurateRulesResponse = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.proposals.len(), 1);
        assert!(parsed.proposals[0].is_new_rule());
        assert!(parsed.proposals[0].rule_draft.is_some());
        parsed.validate().unwrap();
    }

    #[test]
    fn curate_response_rejects_an_active_recommendation() {
        let mut proposal = new_rule_proposal_json();
        proposal["recommended_status"] = json!("active");
        let value = json!({ "proposals": [proposal], "rationale": "x" });
        let parsed: CurateRulesResponse = serde_json::from_value(value).unwrap();
        let err = parsed.validate().unwrap_err();
        assert!(err.contains("active"), "got {err}");
    }

    #[test]
    fn curate_response_requires_a_draft_for_new_rule_and_a_target_otherwise() {
        // new_rule without a draft fails.
        let mut bare = new_rule_proposal_json();
        bare.as_object_mut().unwrap().remove("rule_draft");
        let value = json!({ "proposals": [bare], "rationale": "x" });
        let parsed: CurateRulesResponse = serde_json::from_value(value).unwrap();
        assert!(parsed.validate().is_err());

        // refine_rule WITH a target validates.
        let refine = json!({
            "proposals": [{
                "proposal_type": "refine_rule",
                "risk_level": "medium",
                "title": "Narrow the invoice rule",
                "rationale": "3 overrides for bank mail.",
                "recommended_status": "pending_human_review",
                "target_rule_kind": "action",
                "target_rule_id": "rule_invoice"
            }],
            "rationale": "x"
        });
        let parsed: CurateRulesResponse = serde_json::from_value(refine).unwrap();
        parsed.validate().unwrap();

        // refine_rule WITHOUT a target fails.
        let refine_bare = json!({
            "proposals": [{
                "proposal_type": "refine_rule",
                "risk_level": "medium",
                "title": "Narrow the invoice rule",
                "rationale": "x",
                "recommended_status": "pending_human_review"
            }],
            "rationale": "x"
        });
        let parsed: CurateRulesResponse = serde_json::from_value(refine_bare).unwrap();
        assert!(parsed.validate().is_err());
    }

    #[test]
    fn curate_response_rejects_invented_top_level_fields() {
        let value = json!({
            "proposals": [],
            "rationale": "x",
            "secret_directive": "ship it"
        });
        let parsed: Result<CurateRulesResponse, _> = serde_json::from_value(value);
        assert!(parsed.is_err(), "an invented field must be rejected");
    }
}
