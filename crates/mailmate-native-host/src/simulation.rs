//! The simulation runner: a deterministic **what-if** harness over the real decision spine.
//!
//! Given a rule snapshot and a list of messages, it runs each message through the *same*
//! `classify → plan → guard` path the live host uses ([`PlanningService`] over the cascade,
//! the deterministic action planner, and the hard policy guard) and reports the outcome —
//! with **no** storage, **no** mail mutation, and **no** provider. It is how a maintainer
//! tries a rule change before activating it: feed the candidate rules + representative
//! messages and read back the labels and the would-be plan.
//!
//! The Tier-2 model is a fresh, untrained logistic classifier (indifferent), and no Tier-3
//! provider is attached, so a message no rule classifies degrades to `needs_review` —
//! exactly the safe production posture. The whole run is a pure function of its input.

use std::sync::Arc;

use futures::executor::block_on;
use serde::{Deserialize, Serialize};

use mailmate_common::ids::{AccountId, FolderId, MessageId};
use mailmate_common::mail::{Attachment, MessageData, MessageHeaders};
use mailmate_common::policy::TriggerKind;
use mailmate_common::rules::rule::{EvaluatableRule, RuleKind};
use mailmate_core::PlanningService;
use mailmate_ports::action_planner::ActionPlanner;
use mailmate_ports::classification_engine::ClassificationEngine;
use mailmate_ports::feature_extractor::FeatureExtractor;
use mailmate_ports::policy_guard::PolicyGuard;
use mailmate_ports::rule_engine::RuleEngine;

use mailmate_ml::{DeterministicFeatureExtractor, LogisticRegressionClassifier};
use mailmate_planner::{CascadeClassifier, DefaultActionPlanner};
use mailmate_policy::HardPolicyGuard;
use mailmate_rules::DeterministicRuleEngine;

/// A what-if scenario: the rule set under test and the messages to run through it.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Scenario {
    /// The rules to evaluate (classification + action; split by [`RuleKind`] internally).
    #[serde(default)]
    pub rules: Vec<EvaluatableRule>,
    /// The messages to simulate.
    #[serde(default)]
    pub messages: Vec<SimMessage>,
}

/// A lean message description that lowers into a [`MessageData`] — friendlier to hand-author
/// in a scenario file than the full storage shape.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SimMessage {
    /// An optional stable id (so a tag/move plan has a target); minted if absent.
    #[serde(default)]
    pub id: Option<String>,
    /// The `From` address.
    pub from: String,
    /// The subject.
    #[serde(default)]
    pub subject: String,
    /// The `To` addresses.
    #[serde(default)]
    pub to: Vec<String>,
    /// The body text (retained features are non-body; this only feeds an attached provider,
    /// of which the simulation has none — present for completeness).
    #[serde(default)]
    pub body: Option<String>,
    /// How many attachments the message carries.
    #[serde(default)]
    pub attachments: usize,
}

impl SimMessage {
    /// Lower into the [`MessageData`] the pipeline consumes, with simulation defaults.
    fn into_message_data(self, index: usize) -> MessageData {
        let id = self.id.unwrap_or_else(|| format!("sim_msg_{index}"));
        MessageData {
            id: Some(MessageId::from(id)),
            client_message_id: format!("sim_{index}"),
            account_id: AccountId::from("sim"),
            folder_id: FolderId::from("inbox"),
            thread_id: None,
            headers: MessageHeaders {
                from: self.from,
                to: self.to,
                subject: self.subject,
                ..MessageHeaders::default()
            },
            body_text: self.body,
            attachments: (0..self.attachments)
                .map(|n| Attachment {
                    filename: format!("attachment_{n}"),
                    content_type: "application/octet-stream".to_owned(),
                    size_bytes: 0,
                })
                .collect(),
            remote_content_loaded: false,
            sender_seen_count: None,
            sender_in_address_book: None,
        }
    }
}

/// One message's simulated outcome.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SimOutcome {
    /// The message's internal id.
    pub message_id: String,
    /// The classification labels assigned.
    pub labels: Vec<String>,
    /// Whether the classification flagged it for review.
    pub needs_review: bool,
    /// How many planned actions the guard allowed (low-risk, auto-appliable).
    pub allowed_actions: usize,
    /// How many planned actions the guard held for review.
    pub review_required_actions: usize,
    /// How many candidate actions the guard blocked outright.
    pub blocked_actions: usize,
}

/// The whole run's report.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SimulationReport {
    /// One entry per input message, in order.
    pub outcomes: Vec<SimOutcome>,
}

/// Build the deterministic decision spine ([`PlanningService`] over the cascade, the action
/// planner, and the hard policy guard) seeded from `rules`. The Tier-2 model is fresh and
/// untrained and no Tier-3 provider is attached, so an unclassified message degrades to
/// review — the zero-provider production posture. Shared by the simulation and the benchmark.
#[must_use]
pub fn build_planning(rules: &[EvaluatableRule]) -> PlanningService {
    let classification_rules = rules_of_kind(rules, RuleKind::Classification);
    let action_rules = rules_of_kind(rules, RuleKind::Action);

    let feature_extractor: Arc<dyn FeatureExtractor> =
        Arc::new(DeterministicFeatureExtractor::new());
    let class_engine: Arc<dyn RuleEngine> =
        Arc::new(DeterministicRuleEngine::new(classification_rules));
    let action_engine: Arc<dyn RuleEngine> = Arc::new(DeterministicRuleEngine::new(action_rules));
    let tier2 = Arc::new(LogisticRegressionClassifier::new("spam", "ham"));
    let classifier: Arc<dyn ClassificationEngine> =
        Arc::new(CascadeClassifier::new(class_engine, tier2));
    let planner: Arc<dyn ActionPlanner> = Arc::new(DefaultActionPlanner::new(action_engine));
    let guard: Arc<dyn PolicyGuard> = Arc::new(HardPolicyGuard::new());

    PlanningService::new(feature_extractor, classifier, planner, guard)
}

/// Run `scenario` through the deterministic decision spine and report each message's outcome.
#[must_use]
pub fn run_simulation(scenario: Scenario) -> SimulationReport {
    let planning = build_planning(&scenario.rules);
    let outcomes = scenario
        .messages
        .into_iter()
        .enumerate()
        .map(|(index, message)| simulate_one(&planning, message, index))
        .collect();
    SimulationReport { outcomes }
}

/// Run one message and project its [`PlanningOutcome`](mailmate_core::PlanningOutcome) onto a
/// [`SimOutcome`]. A planning failure (which the deterministic spine does not produce) is
/// reported as a review-flagged outcome rather than aborting the whole run.
fn simulate_one(planning: &PlanningService, message: SimMessage, index: usize) -> SimOutcome {
    let data = message.into_message_data(index);
    let message_id = data
        .id
        .clone()
        .map_or_else(|| format!("sim_msg_{index}"), MessageId::into_string);
    match block_on(planning.handle_message(data, TriggerKind::NewMail)) {
        Ok(outcome) => SimOutcome {
            message_id,
            labels: outcome.classification.labels,
            needs_review: outcome.classification.needs_review,
            allowed_actions: outcome.guarded_plan.allowed_actions.len(),
            review_required_actions: outcome.guarded_plan.review_required_actions.len(),
            blocked_actions: outcome.guarded_plan.blocked_actions.len(),
        },
        Err(_) => SimOutcome {
            message_id,
            labels: vec!["simulation_error".to_owned()],
            needs_review: true,
            allowed_actions: 0,
            review_required_actions: 0,
            blocked_actions: 0,
        },
    }
}

/// The rules of one kind, cloned out of the mixed snapshot.
fn rules_of_kind(rules: &[EvaluatableRule], kind: RuleKind) -> Vec<EvaluatableRule> {
    rules.iter().filter(|r| r.kind == kind).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::ids::{RuleId, RuleVersionId};
    use mailmate_common::rules::condition::Condition;
    use mailmate_common::rules::effect::RuleEffect;
    use mailmate_common::rules::rule::{
        HierarchyBand, RiskLevel, RuleScope, RuleStatus, RuleVersion,
    };

    /// An always-firing classification rule that sets `labels`.
    fn labeling_rule(id: &str, labels: Vec<String>) -> EvaluatableRule {
        EvaluatableRule {
            rule_id: RuleId::from(id),
            kind: RuleKind::Classification,
            scope: RuleScope::Global,
            band: HierarchyBand::LearnedActive,
            status: RuleStatus::Active,
            version: RuleVersion {
                id: RuleVersionId::from("rv_1"),
                version_number: 1,
                condition: Condition::All { all: vec![] },
                effect: RuleEffect {
                    set_labels: labels,
                    ..RuleEffect::default()
                },
                risk_level: RiskLevel::Low,
            },
        }
    }

    fn message(from: &str, subject: &str) -> SimMessage {
        SimMessage {
            id: None,
            from: from.to_owned(),
            subject: subject.to_owned(),
            to: vec![],
            body: None,
            attachments: 0,
        }
    }

    #[test]
    fn a_classification_rule_labels_the_message_at_tier1() {
        let scenario = Scenario {
            rules: vec![labeling_rule("rule_promo", vec!["promo".to_owned()])],
            messages: vec![message("sales@vendor.test", "Big sale")],
        };
        let report = run_simulation(scenario);
        assert_eq!(report.outcomes.len(), 1);
        let outcome = &report.outcomes[0];
        assert_eq!(outcome.labels, vec!["promo".to_owned()]);
        assert!(!outcome.needs_review, "a Tier-1 hit is not review-flagged");
        assert_eq!(outcome.message_id, "sim_msg_0");
    }

    #[test]
    fn an_unclassified_message_degrades_to_needs_review_with_no_provider() {
        let scenario = Scenario {
            rules: vec![],
            messages: vec![message("someone@unknown.test", "hello")],
        };
        let report = run_simulation(scenario);
        assert!(
            report.outcomes[0].needs_review,
            "no rule + no provider → review, never an auto-clear"
        );
    }

    #[test]
    fn the_report_has_one_outcome_per_message_in_order() {
        let scenario = Scenario {
            rules: vec![],
            messages: vec![
                message("a@x.test", "one"),
                message("b@x.test", "two"),
                message("c@x.test", "three"),
            ],
        };
        let report = run_simulation(scenario);
        let ids: Vec<&str> = report
            .outcomes
            .iter()
            .map(|o| o.message_id.as_str())
            .collect();
        assert_eq!(ids, vec!["sim_msg_0", "sim_msg_1", "sim_msg_2"]);
    }

    #[test]
    fn a_scenario_round_trips_through_json() {
        let scenario = Scenario {
            rules: vec![labeling_rule("r", vec!["x".to_owned()])],
            messages: vec![message("a@b.test", "s")],
        };
        let json = serde_json::to_string(&scenario).unwrap();
        let back: Scenario = serde_json::from_str(&json).unwrap();
        let report_a = run_simulation(scenario);
        let report_b = run_simulation(back);
        assert_eq!(report_a, report_b, "simulation is deterministic");
    }
}
