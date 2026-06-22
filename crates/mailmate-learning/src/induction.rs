//! Multi-aspect, interpretable rule induction (Phase 7, 7a/7b).
//!
//! The legacy clustering keyed a candidate on `sender_domain` alone and dropped every row
//! without one. This module instead clusters classification corrections **by their effect**
//! (the corrected label) — independent of domain, keeping domainless rows — and then *induces*
//! the condition: it enumerates candidate predicates from the deterministic feature values the
//! cluster's members share, and greedily composes the `all(...)` clause that best separates the
//! cluster from a **negative pool** (recent mail the candidate also matches but on which the
//! user did *something else*). The objective is back-test precision, so a clause that would
//! mis-fire on the negative pool lowers the score and is not chosen — a false positive is
//! *visible*, never silently admitted.
//!
//! Determinism-first throughout: the induced condition is the same `field`/`op`/`value` AST the
//! Rules manager renders and the deterministic engine executes; no clause calls a model. The
//! search is pure and replayable (same rows in → same clause out), reusing the existing
//! [`back_test`](crate::shadow::back_test) gate as the scoring oracle.

use std::collections::BTreeMap;

use mailmate_common::error::LearningError;
use mailmate_common::features::{FeatureValue, FeatureVector};
use mailmate_common::feedback::{ClassificationFeedbackRow, FeedbackPolarity};
use mailmate_common::ids::AccountId;
use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::rule::{RuleDraft, RuleKind, RuleScope};

use crate::shadow::{back_test, HistoricalExample, ShadowReport};

/// Float comparisons in the greedy search use this tolerance so two equal precisions tie
/// (and fall to the deterministic support/order tie-break) rather than wobble on rounding.
const EPS: f64 = 1e-9;

/// The feature key whose presence binds a cluster (and the rules it induces) to one account.
/// Read forward-compatibly: when capture stamps it, divergent cross-account behavior clusters
/// separately and induces an [`RuleScope::Account`] rule; until then a cluster is account-less
/// and induces a [`RuleScope::Global`] rule.
const ACCOUNT_FEATURE: &str = "account_id";

/// A group of classification **corrections** sharing a corrected label (the *effect*),
/// optionally within one account. Unlike the legacy domain clustering, a row without a sender
/// domain is kept — induction finds whatever deterministic features it shares with its peers.
#[derive(Clone, Debug)]
pub struct EffectCluster {
    /// The account the corrections share, when capture stamped one (else account-agnostic).
    pub account_id: Option<AccountId>,
    /// The corrected label they all assert — the rule's effect axis.
    pub label: String,
    /// The scope an induced rule takes: [`Account`](RuleScope::Account) when account-bound,
    /// else [`Global`](RuleScope::Global) (the cluster is independent of domain).
    pub scope: RuleScope,
    /// The supporting correction rows.
    pub rows: Vec<ClassificationFeedbackRow>,
}

/// The account a classification row is bound to, read from its captured features.
#[must_use]
pub fn account_of(row: &ClassificationFeedbackRow) -> Option<String> {
    match row.salient_features.get(ACCOUNT_FEATURE) {
        Some(FeatureValue::Text(a)) => Some(a.clone()),
        _ => None,
    }
}

/// The `(account, label)` key a classification row clusters under (its effect, scoped to an
/// account when one was captured). The discriminator a negative pool is filtered against.
#[must_use]
pub fn effect_key(row: &ClassificationFeedbackRow) -> (Option<String>, String) {
    (account_of(row), row.human_label.clone())
}

/// Cluster classification **corrections** (negative-polarity rows) by their effect — the
/// `(account, corrected label)` key — keeping rows that have no sender domain. Reinforcements
/// (positive polarity) do not seed a correction cluster. Returns clusters in deterministic
/// `(account, label)` order.
#[must_use]
pub fn cluster_by_effect(rows: Vec<ClassificationFeedbackRow>) -> Vec<EffectCluster> {
    let mut groups: BTreeMap<(Option<String>, String), Vec<ClassificationFeedbackRow>> =
        BTreeMap::new();
    for row in rows {
        if row.polarity != FeedbackPolarity::Negative {
            continue;
        }
        groups.entry(effect_key(&row)).or_default().push(row);
    }
    groups
        .into_iter()
        .map(|((account, label), rows)| {
            let (account_id, scope) = match account {
                Some(a) => (Some(AccountId::from(a)), RuleScope::Account),
                None => (None, RuleScope::Global),
            };
            EffectCluster {
                account_id,
                label,
                scope,
                rows,
            }
        })
        .collect()
}

/// The deterministic field environment a captured feature vector presents to a candidate
/// predicate — every scalar feature lifted into the rule language's value space (the one
/// canonical bridge, [`FieldValue::from_feature`]).
#[must_use]
pub fn feature_fields(fv: &FeatureVector) -> BTreeMap<String, FieldValue> {
    fv.features
        .iter()
        .map(|(k, v)| (k.clone(), FieldValue::from_feature(v)))
        .collect()
}

/// The effect an induced classification rule imposes: set the corrected label.
fn label_effect(label: &str) -> RuleEffect {
    RuleEffect {
        set_labels: vec![label.to_owned()],
        ..RuleEffect::new()
    }
}

/// One back-test example: a row's field environment, with the effect the user actually took.
fn example(fv: &FeatureVector, actual_effect: RuleEffect) -> HistoricalExample {
    HistoricalExample {
        message_id: None,
        fields: feature_fields(fv),
        actual_effect,
    }
}

/// Fold a selected predicate list into a condition: a single predicate stays bare; two or more
/// become an `all(...)` conjunction — exactly the AST the Rules manager renders.
fn clause(preds: &[Predicate]) -> Condition {
    if preds.len() == 1 {
        Condition::Predicate(preds[0].clone())
    } else {
        Condition::All {
            all: preds.iter().cloned().map(Condition::Predicate).collect(),
        }
    }
}

/// Lift a candidate condition into a deterministic classification draft for scoring. The scope
/// is immaterial to a back-test (the engine evaluates the candidate against a scope-less field
/// environment), so a neutral [`Global`](RuleScope::Global) is used here.
fn candidate_draft(condition: &Condition, effect: &RuleEffect) -> RuleDraft {
    RuleDraft {
        kind: RuleKind::Classification,
        scope: RuleScope::Global,
        condition: condition.clone(),
        effect: effect.clone(),
    }
}

/// Enumerate the candidate equality predicates: for every `(feature, value)` a scalar feature
/// takes in at least `min_support` of the positives, a `feature == value` predicate. The
/// structured `Json` escape-hatch has no scalar equality form and is skipped. Values are grouped
/// by their canonical JSON so a float-bearing feature can still be counted. Deterministic order
/// (by `(feature, value-json)`).
fn candidate_predicates(positives: &[FeatureVector], min_support: usize) -> Vec<Predicate> {
    let mut counts: BTreeMap<(String, String), (FeatureValue, usize)> = BTreeMap::new();
    for fv in positives {
        for (key, value) in &fv.features {
            if matches!(value, FeatureValue::Json(_)) {
                continue;
            }
            // `account_id` is a SCOPING dimension, not a condition: account binding is carried by
            // the rule's `RuleScope::Account`, and the production field environment the engine
            // evaluates against never includes `account_id`, so an `account_id == X` predicate
            // would be dead-on-arrival (the rule could never fire). Never make it a candidate.
            if key == ACCOUNT_FEATURE {
                continue;
            }
            let value_key = serde_json::to_string(value).unwrap_or_default();
            let entry = counts
                .entry((key.clone(), value_key))
                .or_insert_with(|| (value.clone(), 0));
            entry.1 += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, (_, count))| *count >= min_support)
        .map(|((field, _), (value, _))| Predicate {
            field,
            op: Operator::Eq,
            value: FieldValue::from_feature(&value),
        })
        .collect()
}

/// Induce the deterministic condition that best explains a cluster's `label` while excluding a
/// negative pool.
///
/// The candidate is back-tested over `positives ++ negatives`, where the negatives carry an
/// *empty* effect (the user did **not** apply this label), so a candidate that fires on a
/// negative scores it as a false positive. Because only a positive can ever agree with the
/// label effect, the report's `correct` count is exactly the **support** (positives matched)
/// and its precision folds in every false positive — one back-test yields both. The search adds
/// the clause that most raises precision while support stays ≥ `min_support`, stopping when no
/// clause strictly improves it or `precision_bar` is reached.
///
/// Returns the induced `(condition, report)` — the report is the *honest* back-test the caller
/// gates on (it may be below `precision_bar`; the caller withholds a low-precision candidate via
/// [`ShadowReport::is_eligible`]). Returns `None` when there is no non-empty candidate that even
/// meets the support floor (the **empty/vacuous-candidate guard**), or when the effect is empty.
///
/// # Errors
/// [`LearningError::Rules`] if the rule engine fails to evaluate a candidate.
pub async fn induce_condition(
    positives: &[FeatureVector],
    negatives: &[FeatureVector],
    label: &str,
    min_support: usize,
    precision_bar: f64,
) -> Result<Option<(Condition, ShadowReport)>, LearningError> {
    let effect = label_effect(label);
    // Empty-effect guard: never score a vacuous effect (a label-less rule would "agree" with
    // everything and score a meaningless precision 1.0).
    if effect.set_labels.is_empty() || positives.is_empty() {
        return Ok(None);
    }

    let mut remaining = candidate_predicates(positives, min_support);
    if remaining.is_empty() {
        return Ok(None);
    }

    // The scoring pool, built once: positives agree with the label; negatives carry an empty
    // effect so a match on one is a visible false positive.
    let pool: Vec<HistoricalExample> = positives
        .iter()
        .map(|fv| example(fv, effect.clone()))
        .chain(negatives.iter().map(|fv| example(fv, RuleEffect::new())))
        .collect();

    let mut selected: Vec<Predicate> = Vec::new();
    let mut best_report: Option<ShadowReport> = None;

    loop {
        // The remaining predicate whose addition yields the best precision while keeping support.
        let mut best: Option<(usize, ShadowReport)> = None;
        for (idx, pred) in remaining.iter().enumerate() {
            let mut trial = selected.clone();
            trial.push(pred.clone());
            let draft = candidate_draft(&clause(&trial), &effect);
            let report = back_test(&draft, &pool).await?;
            // `correct` over this pool == positives matched == support (negatives never agree).
            if report.correct < min_support {
                continue;
            }
            let precision = report.precision().unwrap_or(0.0);
            let better = match &best {
                None => true,
                Some((_, current)) => {
                    let current_p = current.precision().unwrap_or(0.0);
                    precision > current_p + EPS
                        || ((precision - current_p).abs() <= EPS && report.correct > current.correct)
                }
            };
            if better {
                best = Some((idx, report));
            }
        }

        let Some((idx, report)) = best else { break };
        let precision = report.precision().unwrap_or(0.0);
        let prev_precision = best_report.as_ref().and_then(|r| r.precision()).unwrap_or(0.0);
        // Always take a first clause (an empty selection matches nothing); after that, only keep
        // a clause that strictly improves precision — otherwise specializing further just shrinks
        // support for no gain.
        if selected.is_empty() || precision > prev_precision + EPS {
            selected.push(remaining.remove(idx));
            best_report = Some(report);
            if precision >= precision_bar {
                break;
            }
        } else {
            break;
        }
    }

    match best_report {
        Some(report) if !selected.is_empty() => Ok(Some((clause(&selected), report))),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use mailmate_common::feedback::{ClassificationFeedback, PinnedVersions};
    use mailmate_common::ids::MessageId;
    use mailmate_common::time::Timestamp;

    fn fv(pairs: &[(&str, FeatureValue)]) -> FeatureVector {
        let mut v = FeatureVector::new();
        for (k, val) in pairs {
            v.insert(*k, val.clone());
        }
        v
    }

    fn corr(label: &str, features: FeatureVector) -> ClassificationFeedbackRow {
        ClassificationFeedbackRow {
            id: ClassificationFeedback::fresh_id(),
            message_id: MessageId::fresh(),
            pinned_versions: PinnedVersions::default(),
            ai_label: Some("not_junk".to_owned()),
            ai_score: None,
            ai_rationale: None,
            human_label: label.to_owned(),
            human_reason_code: None,
            human_reason_text: None,
            salient_features: features,
            polarity: FeedbackPolarity::Negative,
            created_at: Timestamp::now(),
        }
    }

    // Exit example: `auth_fail AND no_prior_contact → suspicious`. Neither single clause
    // separates the cluster from the negative pool; their conjunction does.
    #[test]
    fn induces_a_two_clause_rule_that_excludes_the_negative_pool() {
        let positives = vec![
            fv(&[
                ("auth_result", FeatureValue::Text("fail".into())),
                ("no_prior_contact", FeatureValue::Bool(true)),
            ]),
            fv(&[
                ("auth_result", FeatureValue::Text("fail".into())),
                ("no_prior_contact", FeatureValue::Bool(true)),
            ]),
            fv(&[
                ("auth_result", FeatureValue::Text("fail".into())),
                ("no_prior_contact", FeatureValue::Bool(true)),
            ]),
        ];
        let negatives = vec![
            // A known contact whose mail failed auth — auth_result=fail alone would mis-fire here.
            fv(&[
                ("auth_result", FeatureValue::Text("fail".into())),
                ("no_prior_contact", FeatureValue::Bool(false)),
            ]),
            // A stranger who passed auth — no_prior_contact=true alone would mis-fire here.
            fv(&[
                ("auth_result", FeatureValue::Text("pass".into())),
                ("no_prior_contact", FeatureValue::Bool(true)),
            ]),
        ];
        let (cond, report) =
            block_on(induce_condition(&positives, &negatives, "suspicious", 3, 0.9))
                .unwrap()
                .expect("a separating conjunction exists");
        // Two clauses, both present.
        match &cond {
            Condition::All { all } => {
                assert_eq!(all.len(), 2, "a two-clause conjunction: {cond:?}");
                let fields: Vec<&str> = all
                    .iter()
                    .filter_map(|c| match c {
                        Condition::Predicate(p) => Some(p.field.as_str()),
                        _ => None,
                    })
                    .collect();
                assert!(fields.contains(&"auth_result"), "{fields:?}");
                assert!(fields.contains(&"no_prior_contact"), "{fields:?}");
            }
            other => panic!("expected a two-clause All, got {other:?}"),
        }
        // Honest precision: the conjunction fires on the 3 positives only → 1.0, support 3.
        assert_eq!(report.support(), 3);
        assert_eq!(report.precision(), Some(1.0));
    }

    #[test]
    fn a_single_sufficient_predicate_induces_a_one_clause_rule() {
        let positives = vec![
            fv(&[("sender_domain", FeatureValue::Text("stripe.com".into()))]),
            fv(&[("sender_domain", FeatureValue::Text("stripe.com".into()))]),
            fv(&[("sender_domain", FeatureValue::Text("stripe.com".into()))]),
        ];
        // A negative pool the single predicate already excludes.
        let negatives = vec![fv(&[("sender_domain", FeatureValue::Text("other.com".into()))])];
        let (cond, report) =
            block_on(induce_condition(&positives, &negatives, "receipts", 3, 0.9))
                .unwrap()
                .unwrap();
        match cond {
            Condition::Predicate(p) => {
                assert_eq!(p.field, "sender_domain");
                assert_eq!(p.value, FieldValue::Text("stripe.com".into()));
            }
            other => panic!("expected a single bare predicate, got {other:?}"),
        }
        assert_eq!(report.precision(), Some(1.0));
    }

    #[test]
    fn returns_the_best_effort_condition_with_honest_low_precision_for_the_caller_to_gate() {
        // Positives and negatives are indistinguishable on the only shared feature, so the best
        // (only) candidate fires on both: induction returns it with an honest 0.5 precision —
        // the engine, not the search, withholds it via the precision bar.
        let positives = vec![
            fv(&[("auth_result", FeatureValue::Text("fail".into()))]),
            fv(&[("auth_result", FeatureValue::Text("fail".into()))]),
        ];
        let negatives = vec![
            fv(&[("auth_result", FeatureValue::Text("fail".into()))]),
            fv(&[("auth_result", FeatureValue::Text("fail".into()))]),
        ];
        let (_, report) = block_on(induce_condition(&positives, &negatives, "suspicious", 2, 0.9))
            .unwrap()
            .unwrap();
        // The induced-rule support is `correct` (the positives reproduced) — NOT `fires`, which
        // here also counts the two negative-pool false positives. The card must show the honest
        // positive support, never inflate it with mail the rule got wrong.
        assert_eq!(report.correct, 2, "reproduced the two positives");
        assert_eq!(report.fires, 4, "but also fired on the two negatives");
        assert_eq!(report.precision(), Some(0.5), "so precision is an honest 0.5");
        // The caller gates on precision ≥ bar AND positive support ≥ floor; 0.5 < 0.9 withholds it.
        assert!(report.precision().unwrap() < 0.9);
    }

    #[test]
    fn no_shared_feature_meets_support_so_nothing_is_induced() {
        // Three positives, each with a distinct value — no value reaches the support floor.
        let positives = vec![
            fv(&[("sender_domain", FeatureValue::Text("a.com".into()))]),
            fv(&[("sender_domain", FeatureValue::Text("b.com".into()))]),
            fv(&[("sender_domain", FeatureValue::Text("c.com".into()))]),
        ];
        let induced =
            block_on(induce_condition(&positives, &[], "x", 3, 0.9)).unwrap();
        assert!(induced.is_none(), "the empty/vacuous-candidate guard holds");
    }

    #[test]
    fn cluster_by_effect_keeps_domainless_rows_and_groups_by_label() {
        let rows = vec![
            // No sender domain at all — the legacy clustering dropped these; here they cluster.
            corr("suspicious", fv(&[("auth_result", FeatureValue::Text("fail".into()))])),
            corr("suspicious", fv(&[("auth_result", FeatureValue::Text("fail".into()))])),
            corr("newsletter", fv(&[("list_id", FeatureValue::Text("news".into()))])),
            // A reinforcement does not seed a correction cluster.
            {
                let mut r = corr("suspicious", fv(&[("auth_result", FeatureValue::Text("fail".into()))]));
                r.polarity = FeedbackPolarity::Positive;
                r
            },
        ];
        let clusters = cluster_by_effect(rows);
        assert_eq!(clusters.len(), 2, "two labels; domainless kept, reinforcement dropped");
        let suspicious = clusters.iter().find(|c| c.label == "suspicious").unwrap();
        assert_eq!(suspicious.rows.len(), 2, "only the two negative corrections");
        assert_eq!(suspicious.scope, RuleScope::Global, "account-less → global");
        assert!(suspicious.account_id.is_none());
    }

    #[test]
    fn account_id_is_never_induced_as_a_predicate_only_as_scope() {
        // account_id binds via RuleScope::Account, not a predicate — the runtime field environment
        // never carries it, so an `account_id == X` clause would be dead-on-arrival. Even though
        // every positive shares the same account, the induced condition keys on the REAL feature.
        let positives = vec![
            fv(&[
                ("account_id", FeatureValue::Text("work".into())),
                ("auth_result", FeatureValue::Text("fail".into())),
            ]),
            fv(&[
                ("account_id", FeatureValue::Text("work".into())),
                ("auth_result", FeatureValue::Text("fail".into())),
            ]),
        ];
        let negatives = vec![fv(&[
            ("account_id", FeatureValue::Text("work".into())),
            ("auth_result", FeatureValue::Text("pass".into())),
        ])];
        let (cond, _) = block_on(induce_condition(&positives, &negatives, "suspicious", 2, 0.9))
            .unwrap()
            .expect("auth_result separates the pool");
        // The induced predicate is auth_result — never account_id.
        match cond {
            Condition::Predicate(p) => assert_eq!(p.field, "auth_result"),
            Condition::All { all } => {
                let fields: Vec<&str> = all
                    .iter()
                    .filter_map(|c| match c {
                        Condition::Predicate(p) => Some(p.field.as_str()),
                        _ => None,
                    })
                    .collect();
                assert!(!fields.contains(&"account_id"), "account_id must not be a predicate: {fields:?}");
            }
            other => panic!("unexpected condition {other:?}"),
        }
    }

    #[test]
    fn an_account_stamped_cluster_scopes_to_that_account() {
        let rows = vec![
            corr(
                "suspicious",
                fv(&[
                    ("account_id", FeatureValue::Text("work".into())),
                    ("auth_result", FeatureValue::Text("fail".into())),
                ]),
            ),
            corr(
                "suspicious",
                fv(&[
                    ("account_id", FeatureValue::Text("work".into())),
                    ("auth_result", FeatureValue::Text("fail".into())),
                ]),
            ),
        ];
        let clusters = cluster_by_effect(rows);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].scope, RuleScope::Account);
        assert_eq!(clusters[0].account_id.as_ref().unwrap().as_str(), "work");
    }
}
