//! A dependency-free micro-benchmark harness for MailMate's hot paths.
//!
//! No `criterion`, no nightly `#[bench]` — just [`Instant`] timing of the deterministic
//! decision spine, so `cargo test` covers it and the `bench` subcommand prints a report. It
//! measures the operations that run per message at steady state: deterministic feature
//! extraction and the full `classify → plan → guard` path (with and without a matching rule).
//! Numbers are indicative throughput on the running machine, not a regression gate.

use std::time::Instant;

use serde::Serialize;

use futures::executor::block_on;
use mailmate_common::ids::{AccountId, FolderId, MessageId};
use mailmate_common::mail::{MessageData, MessageHeaders};
use mailmate_common::policy::TriggerKind;
use mailmate_common::rules::condition::Condition;
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::rule::{
    EvaluatableRule, HierarchyBand, RiskLevel, RuleKind, RuleScope, RuleStatus, RuleVersion,
};
use mailmate_ports::feature_extractor::FeatureExtractor;

use mailmate_ml::DeterministicFeatureExtractor;

use crate::simulation::build_planning;

/// One benchmarked operation's result.
#[derive(Clone, Debug, Serialize)]
pub struct BenchResult {
    /// The operation name.
    pub name: String,
    /// How many iterations were timed.
    pub iterations: u32,
    /// Total wall-clock nanoseconds across all iterations.
    pub total_nanos: u128,
    /// Mean nanoseconds per operation.
    pub per_op_nanos: u128,
}

impl BenchResult {
    /// A human-readable one-line summary.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{:<28} {:>8} iters  {:>10} ns/op",
            self.name, self.iterations, self.per_op_nanos
        )
    }
}

/// Time `op` over `iterations` runs, returning the aggregate. `iterations` is clamped to at
/// least 1 so `per_op_nanos` never divides by zero.
pub fn time_it(name: &str, iterations: u32, mut op: impl FnMut()) -> BenchResult {
    let iters = iterations.max(1);
    let start = Instant::now();
    for _ in 0..iters {
        op();
    }
    let total_nanos = start.elapsed().as_nanos();
    BenchResult {
        name: name.to_owned(),
        iterations: iters,
        total_nanos,
        per_op_nanos: total_nanos / u128::from(iters),
    }
}

/// A representative message to push through the spine.
fn sample_message(from: &str, subject: &str) -> MessageData {
    MessageData {
        id: Some(MessageId::from("bench_msg")),
        client_message_id: "bench".to_owned(),
        account_id: AccountId::from("bench"),
        folder_id: FolderId::from("inbox"),
        thread_id: None,
        headers: MessageHeaders {
            from: from.to_owned(),
            subject: subject.to_owned(),
            ..MessageHeaders::default()
        },
        body_text: Some("a representative body of moderate length".to_owned()),
        attachments: vec![],
        remote_content_loaded: false,
    }
}

/// An always-firing classification rule labelling `promo` — the "rule hits at Tier 1" path.
fn promo_rule() -> EvaluatableRule {
    EvaluatableRule {
        rule_id: mailmate_common::ids::RuleId::from("rule_bench_promo"),
        kind: RuleKind::Classification,
        scope: RuleScope::Global,
        band: HierarchyBand::LearnedActive,
        status: RuleStatus::Active,
        version: RuleVersion {
            id: mailmate_common::ids::RuleVersionId::from("rv_bench"),
            version_number: 1,
            condition: Condition::All { all: vec![] },
            effect: RuleEffect {
                set_labels: vec!["promo".to_owned()],
                ..RuleEffect::default()
            },
            risk_level: RiskLevel::Low,
        },
    }
}

/// Run the default benchmark suite at `iterations` per operation.
#[must_use]
pub fn run_default_suite(iterations: u32) -> Vec<BenchResult> {
    let extractor = DeterministicFeatureExtractor::new();
    let message = sample_message("rep@vendor.test", "Quarterly update");

    let feature_bench = time_it("feature_extraction", iterations, || {
        let _ = extractor.extract(&message);
    });

    // No rules: the Tier-2 + degrade-to-review path.
    let cold_planning = build_planning(&[]);
    let classify_cold = time_it("classify_plan_guard_no_rules", iterations, || {
        let _ = block_on(cold_planning.handle_message(message.clone(), TriggerKind::NewMail));
    });

    // One rule: the Tier-1 short-circuit path.
    let warm_planning = build_planning(&[promo_rule()]);
    let classify_warm = time_it("classify_plan_guard_tier1_hit", iterations, || {
        let _ = block_on(warm_planning.handle_message(message.clone(), TriggerKind::NewMail));
    });

    vec![feature_bench, classify_cold, classify_warm]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_it_clamps_zero_iterations_and_reports_a_per_op() {
        let result = time_it("noop", 0, || {});
        assert_eq!(result.iterations, 1, "zero is clamped to one");
        // per_op is total/iters with iters >= 1, so it never panics.
        assert_eq!(result.per_op_nanos, result.total_nanos);
    }

    #[test]
    fn the_default_suite_benchmarks_the_three_hot_paths() {
        let results = run_default_suite(3);
        let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "feature_extraction",
                "classify_plan_guard_no_rules",
                "classify_plan_guard_tier1_hit"
            ]
        );
        for result in &results {
            assert_eq!(result.iterations, 3);
            assert!(!result.summary().is_empty());
        }
    }
}
