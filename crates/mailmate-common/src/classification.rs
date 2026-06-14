//! Pipeline-1 (classify / understand) output vocabulary: the cascade's verdict on a
//! message, how it was reached (tier + provenance), and the versioned confidence bands
//! that decide escalation.
//!
//! A [`Classification`] is *prediction* — "what is this email" — distinct from the P2
//! [`ActionPlan`](crate::action::ActionPlan) decision "what should we do about it". It is
//! produced by the `ClassificationEngine` port (default adapter: the three-tier cascade)
//! and consumed by the `ActionPlanner` and the policy guard.

use serde::{Deserialize, Serialize};

use crate::features::FeatureVector;
use crate::ids::{DecisionId, MessageId, RuleId};
use crate::mail::MessageData;
use crate::policy::MailCategory;

/// Message priority a classifier may assign. The first-class domain type; the AI
/// `classify_email` schema re-exports it so the model vocabulary and the domain agree.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// Low priority.
    Low,
    /// Normal priority (the conservative default when no signal raises it).
    #[default]
    Normal,
    /// High priority.
    High,
    /// Urgent.
    Urgent,
}

impl Priority {
    /// The stable snake_case label used on the wire, in rule effects, and in audit rows.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
            Self::Urgent => "urgent",
        }
    }

    /// Parse a priority from its label, falling back to [`Normal`](Priority::Normal) for an
    /// unknown value — a rule effect carrying a typo degrades conservatively, never panics.
    #[must_use]
    pub fn from_label(label: &str) -> Self {
        match label {
            "low" => Self::Low,
            "high" => Self::High,
            "urgent" => Self::Urgent,
            _ => Self::Normal,
        }
    }
}

/// Which tier of the cascade decided a classification.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationTier {
    /// Tier 1 — deterministic signals + classification rules (zero model inference).
    Tier1Rules,
    /// Tier 2 — the local `Tier2Classifier` over deterministic features.
    Tier2Model,
    /// Tier 3 — the LLM `classify_email` task (only reached on escalation).
    Tier3Llm,
}

impl ClassificationTier {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tier1Rules => "tier1_rules",
            Self::Tier2Model => "tier2_model",
            Self::Tier3Llm => "tier3_llm",
        }
    }
}

/// How a classification was reached — the audit/explain trail behind the verdict.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ClassificationProvenance {
    /// The tier that produced the final verdict.
    pub tier: ClassificationTier,
    /// Whether the message escalated past a cheaper tier to reach `tier`.
    pub escalated: bool,
    /// The calibration table behind a Tier-2 score, if Tier 2 decided it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration_version: Option<String>,
    /// The provider that answered a Tier-3 call, if Tier 3 decided it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// The classification rules that fired at Tier 1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fired_rules: Vec<RuleId>,
}

impl ClassificationProvenance {
    /// A Tier-1 (deterministic-rule) provenance.
    #[must_use]
    pub fn tier1(fired_rules: Vec<RuleId>) -> Self {
        Self {
            tier: ClassificationTier::Tier1Rules,
            escalated: false,
            calibration_version: None,
            provider_id: None,
            fired_rules,
        }
    }

    /// A Tier-2 (local-model) provenance.
    #[must_use]
    pub fn tier2(calibration_version: impl Into<String>, escalated: bool) -> Self {
        Self {
            tier: ClassificationTier::Tier2Model,
            escalated,
            calibration_version: Some(calibration_version.into()),
            provider_id: None,
            fired_rules: Vec::new(),
        }
    }

    /// A Tier-3 (LLM) provenance.
    #[must_use]
    pub fn tier3(provider_id: impl Into<String>) -> Self {
        Self {
            tier: ClassificationTier::Tier3Llm,
            escalated: true,
            calibration_version: None,
            provider_id: Some(provider_id.into()),
            fired_rules: Vec::new(),
        }
    }
}

/// The P1 verdict on a message: labels, the two safety scores, a priority, and how it was
/// decided. `needs_review` is set when escalation was *needed* but no Tier-3 provider was
/// available — the message degrades to review rather than being auto-cleared.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Classification {
    /// The decision this classification belongs to.
    pub decision_id: DecisionId,
    /// Assigned labels (free-form; rules and downstream policy key off these).
    #[serde(default)]
    pub labels: Vec<String>,
    /// Spam confidence in `[0, 1]`.
    pub spam_score: f64,
    /// Phishing confidence in `[0, 1]`.
    pub phishing_score: f64,
    /// Assigned priority.
    pub priority: Priority,
    /// Whether the cascade could not confidently clear the message and a provider was
    /// unavailable to escalate to — the message must be surfaced for human review.
    #[serde(default)]
    pub needs_review: bool,
    /// How the verdict was reached.
    pub provenance: ClassificationProvenance,
}

impl Classification {
    /// The sensitive mail categories implied by this classification's labels — the signal
    /// the `financial_security_legal_move_requires_review` policy keys off.
    ///
    /// A deterministic starter map (learned rules and curation refine label→category over
    /// time); unknown labels imply no elevated handling.
    #[must_use]
    pub fn policy_categories(&self) -> Vec<MailCategory> {
        let mut categories = Vec::new();
        for label in &self.labels {
            let category = match label.as_str() {
                "financial" | "invoice" | "payment" | "receipt" | "banking" => {
                    Some(MailCategory::Financial)
                }
                "security" | "phishing" | "account_alert" | "password_reset" | "two_factor" => {
                    Some(MailCategory::Security)
                }
                "legal" | "contract" | "notice" => Some(MailCategory::Legal),
                _ => None,
            };
            if let Some(category) = category {
                if !categories.contains(&category) {
                    categories.push(category);
                }
            }
        }
        categories
    }

    /// Whether any implied category is sensitive (financial / security / legal).
    #[must_use]
    pub fn is_sensitive(&self) -> bool {
        self.policy_categories()
            .iter()
            .copied()
            .any(MailCategory::is_sensitive)
    }
}

/// The cascade's input: the message and its deterministic feature vector, keyed to a
/// decision. The feature vector is produced by the (pure) `FeatureExtractor` so Tier 1/2
/// never touch message bodies and the back-test is replayable.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ClassificationInput {
    /// The decision this classification belongs to.
    pub decision_id: DecisionId,
    /// The message under classification.
    pub message: MessageData,
    /// Its deterministic, non-body features.
    pub features: FeatureVector,
}

impl ClassificationInput {
    /// Build an input for `message` with its extracted `features`, minting a fresh
    /// decision id.
    #[must_use]
    pub fn new(message: MessageData, features: FeatureVector) -> Self {
        Self {
            decision_id: DecisionId::fresh(),
            message,
            features,
        }
    }

    /// The message's resolved internal id, if it has been persisted.
    #[must_use]
    pub fn message_id(&self) -> Option<&MessageId> {
        self.message.id.as_ref()
    }
}

/// Versioned confidence bands governing cascade escalation. Conservative at cold-start: a
/// high acceptance bar and a low phishing-clearance floor mean the untrained Tier-2 model
/// escalates often, and the escalation rate falls automatically as the model is trained
/// (everything versioned, like the rest of the cascade).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CascadeThresholds {
    /// The band-set version (stamped into provenance for replay).
    pub version: String,
    /// Minimum calibrated top-label confidence for Tier 2 to accept without escalating.
    pub tier2_accept: f64,
    /// The asymmetric safety floor: Tier 1/2 may clear a message only if its phishing score
    /// is at or below this. A higher phishing score escalates even when the top-label
    /// confidence clears `tier2_accept` ("clearly safe" needs high confidence; "possibly
    /// phishing" escalates readily).
    pub phishing_safe_floor: f64,
}

impl Default for CascadeThresholds {
    fn default() -> Self {
        Self {
            version: "cascade-bands-v1".to_owned(),
            tier2_accept: 0.85,
            phishing_safe_floor: 0.20,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classification(labels: &[&str]) -> Classification {
        Classification {
            decision_id: DecisionId::from("dec_1"),
            labels: labels.iter().map(|s| (*s).to_owned()).collect(),
            spam_score: 0.0,
            phishing_score: 0.0,
            priority: Priority::Normal,
            needs_review: false,
            provenance: ClassificationProvenance::tier1(vec![]),
        }
    }

    #[test]
    fn priority_label_round_trips_and_defaults_conservatively() {
        for p in [
            Priority::Low,
            Priority::Normal,
            Priority::High,
            Priority::Urgent,
        ] {
            assert_eq!(Priority::from_label(p.as_str()), p);
        }
        assert_eq!(Priority::from_label("nonsense"), Priority::Normal);
        assert_eq!(Priority::default(), Priority::Normal);
    }

    #[test]
    fn labels_map_to_sensitive_categories() {
        assert_eq!(
            classification(&["invoice"]).policy_categories(),
            vec![MailCategory::Financial]
        );
        assert!(classification(&["phishing"]).is_sensitive());
        assert!(classification(&["contract"]).is_sensitive());
        assert!(!classification(&["newsletter"]).is_sensitive());
        // Duplicate categories collapse.
        assert_eq!(
            classification(&["invoice", "payment"]).policy_categories(),
            vec![MailCategory::Financial]
        );
    }

    #[test]
    fn provenance_constructors_set_the_tier() {
        assert_eq!(
            ClassificationProvenance::tier1(vec![]).tier,
            ClassificationTier::Tier1Rules
        );
        let t2 = ClassificationProvenance::tier2("logreg-identity-v1", true);
        assert_eq!(t2.tier, ClassificationTier::Tier2Model);
        assert!(t2.escalated);
        assert_eq!(
            t2.calibration_version.as_deref(),
            Some("logreg-identity-v1")
        );
        let t3 = ClassificationProvenance::tier3("mock");
        assert_eq!(t3.tier, ClassificationTier::Tier3Llm);
        assert!(t3.escalated);
        assert_eq!(t3.provider_id.as_deref(), Some("mock"));
    }

    #[test]
    fn tier_labels_are_stable() {
        assert_eq!(ClassificationTier::Tier1Rules.as_str(), "tier1_rules");
        assert_eq!(ClassificationTier::Tier2Model.as_str(), "tier2_model");
        assert_eq!(ClassificationTier::Tier3Llm.as_str(), "tier3_llm");
    }

    #[test]
    fn thresholds_default_is_conservative_and_classification_round_trips() {
        let t = CascadeThresholds::default();
        assert_eq!(t.version, "cascade-bands-v1");
        assert!(t.tier2_accept > 0.8);
        assert!(t.phishing_safe_floor < 0.5);

        let c = classification(&["invoice"]);
        let json = serde_json::to_string(&c).unwrap();
        let back: Classification = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }
}
