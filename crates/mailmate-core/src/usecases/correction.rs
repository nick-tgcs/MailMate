//! The correction use-case: a user teaching the system.
//!
//! A [`UserCorrection`] is the raw material of the learning loop. This service does the two
//! things a correction triggers, in order: it captures the correction into its single-owner
//! per-task feedback table (via the [`LearningEngine`] port), and — for a spam-axis signal —
//! it feeds the [`Tier2Classifier`] an online update so the not-yet-crystallized residual
//! improves immediately. It names only ports, so the learning engine and the classifier are
//! injected at the edge.
//!
//! Determinism-first: the feedback row is the durable, replayable record; the Tier-2 update
//! is the *teacher/fallback* path. The crystallization of a repeated correction into a
//! model-free rule is the learning engine's later job — not done here.

use std::sync::Arc;

use mailmate_common::correction::UserCorrection;
use mailmate_common::error::MailMateError;
use mailmate_common::features::{FeatureValue, FeatureVector};
use mailmate_common::feedback::{
    ClassificationFeedback, ClassificationFeedbackRow, FeedbackPolarity, FilingFeedback,
    FilingFeedbackRow, PinnedVersions, TaskFeedback,
};
use mailmate_common::ids::{FeedbackId, FolderId};
use mailmate_common::time::Timestamp;
use mailmate_ports::learning_engine::LearningEngine;
use mailmate_ports::tier2_classifier::Tier2Classifier;

use crate::Ports;

/// The fallback `human_label` for a rejected-signal correction when no prior AI label is known —
/// the signal id itself rides in `human_reason_code`, so the row stays unambiguous.
const SIGNAL_MARKED_WRONG_LABEL: &str = "signal_marked_wrong";

/// The provenance/prediction context for a correction — what the AI had predicted (so the
/// feedback row records divergence and polarity) and the sender domain (so the captured row
/// is clusterable by the learning engine). Everything is optional; an empty context records
/// the human's choice as a plain override.
#[derive(Clone, Debug, Default)]
pub struct CorrectionContext {
    /// The sender's domain, retained for deterministic domain-clustered proposals.
    pub sender_domain: Option<String>,
    /// The account the corrected message belongs to, folded into a classification correction's
    /// salient features so multi-aspect induction can cluster *per account* and scope a learned
    /// rule to [`RuleScope::Account`](mailmate_common::rules::rule::RuleScope::Account). When
    /// absent, induction clusters account-agnostically and the rule degrades to `Global` — honest,
    /// never a false account scope.
    pub account_id: Option<String>,
    /// The label the AI had assigned on the spam axis (`spam`/`ham`/…), if any.
    pub ai_label: Option<String>,
    /// The AI's confidence, if a model produced the prediction.
    pub ai_score: Option<f64>,
    /// The folder the AI had suggested for a filing correction, if any.
    pub ai_suggested_folder: Option<FolderId>,
    /// The versions that produced the corrected prediction.
    pub pinned_versions: PinnedVersions,
}

/// Captures user corrections and feeds the Tier-2 online update.
#[derive(Clone)]
pub struct CorrectionService {
    learning_engine: Arc<dyn LearningEngine>,
    tier2: Arc<dyn Tier2Classifier>,
}

impl CorrectionService {
    /// Assemble the service from its two ports.
    #[must_use]
    pub fn new(learning_engine: Arc<dyn LearningEngine>, tier2: Arc<dyn Tier2Classifier>) -> Self {
        Self {
            learning_engine,
            tier2,
        }
    }

    /// Assemble the service from the core's [`Ports`] dependency bundle.
    #[must_use]
    pub fn from_ports(ports: &Ports) -> Self {
        Self::new(ports.learning_engine.clone(), ports.tier2.clone())
    }

    /// Capture `correction` into its per-task feedback table and, for a spam-axis signal,
    /// apply the Tier-2 online update. Returns the new feedback-row id.
    ///
    /// # Errors
    /// Propagates a learning-engine capture failure or a Tier-2 update failure as a
    /// [`MailMateError`].
    pub async fn handle_correction(
        &self,
        correction: UserCorrection,
        features: FeatureVector,
        context: CorrectionContext,
    ) -> Result<FeedbackId, MailMateError> {
        let feedback = build_feedback(&correction, &features, &context);
        let feedback_id = self.learning_engine.record_feedback(feedback).await?;

        // A spam-axis correction also teaches the Tier-2 classifier online (the fallback for
        // the not-yet-crystallized residual). A filing correction trains no spam model.
        if let Some(example) = correction.to_labeled_example(features) {
            self.tier2.update(example).await?;
        }

        Ok(feedback_id)
    }
}

/// Build the single-owner feedback row for a correction.
fn build_feedback(
    correction: &UserCorrection,
    features: &FeatureVector,
    context: &CorrectionContext,
) -> TaskFeedback {
    match correction {
        UserCorrection::MarkSpam { message_id } | UserCorrection::MarkNotSpam { message_id } => {
            let human_label = correction
                .spam_label()
                .expect("a spam-axis correction always has a spam label")
                .to_owned();
            TaskFeedback::Classification(classification_row(
                message_id,
                human_label,
                features,
                context,
            ))
        }
        // A wrong-category correction teaches the same classification-feedback table with an
        // arbitrary label (e.g. "newsletters"); `ai_label` carries the prior label so the
        // polarity records the override honestly. It is a category signal, so — unlike the
        // spam axis — `handle_correction` does not feed it to the Tier-2 online update.
        UserCorrection::CorrectLabel { message_id, label } => TaskFeedback::Classification(
            classification_row(message_id, label.clone(), features, context),
        ),
        // A tag add/remove teaches the same classification table under the tag key, but its
        // polarity comes from the DIRECTION (add = positive, remove = negative), independent of
        // any prior AI label — so an explicit tag is a clean, first-class category signal.
        UserCorrection::TagChanged {
            message_id,
            tag,
            added,
        } => {
            let mut row = classification_row(message_id, tag.clone(), features, context);
            row.polarity = if *added {
                FeedbackPolarity::Positive
            } else {
                FeedbackPolarity::Negative
            };
            TaskFeedback::Classification(row)
        }
        // "This reason is wrong": a rejected salient signal. It teaches a negative
        // classification-feedback row whose `human_reason_code` is the signal id (a feature key
        // or rule id) and whose label echoes the prior verdict, so a feature the user keeps
        // rejecting is durable evidence against any rule that would lean on it. Negative by
        // construction (the user is removing support, not asserting a new label).
        UserCorrection::SignalMarkedWrong {
            message_id,
            signal_id,
        } => {
            let label = context
                .ai_label
                .clone()
                .unwrap_or_else(|| SIGNAL_MARKED_WRONG_LABEL.to_owned());
            let mut row = classification_row(message_id, label, features, context);
            row.human_reason_code = Some(signal_id.clone());
            row.polarity = FeedbackPolarity::Negative;
            TaskFeedback::Classification(row)
        }
        UserCorrection::LearnFiling {
            message_id,
            to_folder,
        } => {
            let polarity = polarity(context.ai_suggested_folder.as_ref() == Some(to_folder));
            TaskFeedback::Filing(FilingFeedbackRow {
                id: FilingFeedback::fresh_id(),
                message_id: message_id.clone(),
                pinned_versions: context.pinned_versions.clone(),
                sender_domain: context.sender_domain.clone(),
                ai_suggested_folder: context.ai_suggested_folder.clone(),
                human_chosen_folder: to_folder.clone(),
                basis: context.sender_domain.as_ref().map(|_| "domain".to_owned()),
                matched_rule_id: None,
                polarity,
                created_at: Timestamp::now(),
            })
        }
    }
}

/// Build a classification-feedback row teaching `human_label` for `message_id`, folding the
/// sender domain into the salient features and recording polarity against any prior AI label.
/// Shared by the spam-axis corrections and the wrong-category correction.
fn classification_row(
    message_id: &mailmate_common::ids::MessageId,
    human_label: String,
    features: &FeatureVector,
    context: &CorrectionContext,
) -> ClassificationFeedbackRow {
    // A correction the AI already agreed with is reinforcement; otherwise an override.
    let polarity = polarity(context.ai_label.as_deref() == Some(human_label.as_str()));
    let mut salient = features.clone();
    if let Some(domain) = &context.sender_domain {
        salient.insert("sender_domain", FeatureValue::Text(domain.clone()));
    }
    // Fold the account into the salient features (key `account_id`) so multi-aspect induction can
    // cluster per account. It is a SCOPING dimension, not a predicate: induction reads it to choose
    // `RuleScope::Account` but never emits an `account_id == X` clause (the runtime field
    // environment carries no account_id, so such a clause would be dead on arrival).
    if let Some(account) = &context.account_id {
        salient.insert("account_id", FeatureValue::Text(account.clone()));
    }
    ClassificationFeedbackRow {
        id: ClassificationFeedback::fresh_id(),
        message_id: message_id.clone(),
        pinned_versions: context.pinned_versions.clone(),
        ai_label: context.ai_label.clone(),
        ai_score: context.ai_score,
        ai_rationale: None,
        human_label,
        human_reason_code: None,
        human_reason_text: None,
        salient_features: salient,
        polarity,
        created_at: Timestamp::now(),
    }
}

/// A correction the AI agreed with is `Positive` reinforcement; a divergence is `Negative`.
fn polarity(ai_matched_human: bool) -> FeedbackPolarity {
    if ai_matched_human {
        FeedbackPolarity::Positive
    } else {
        FeedbackPolarity::Negative
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::ids::MessageId;

    fn ctx_with_domain(domain: &str) -> CorrectionContext {
        CorrectionContext {
            sender_domain: Some(domain.to_owned()),
            ..CorrectionContext::default()
        }
    }

    #[test]
    fn spam_correction_builds_a_classification_row_with_injected_domain() {
        let correction = UserCorrection::MarkSpam {
            message_id: MessageId::from("msg_1"),
        };
        let feedback = build_feedback(
            &correction,
            &FeatureVector::new(),
            &ctx_with_domain("x.com"),
        );
        match feedback {
            TaskFeedback::Classification(row) => {
                assert_eq!(row.human_label, "spam");
                // No prior AI label → this is a divergence (negative).
                assert_eq!(row.polarity, FeedbackPolarity::Negative);
                assert_eq!(
                    row.salient_features.get("sender_domain"),
                    Some(&FeatureValue::Text("x.com".to_owned()))
                );
            }
            TaskFeedback::Filing(_) => panic!("expected a classification row"),
        }
    }

    #[test]
    fn a_correction_folds_the_account_into_salient_features_for_per_account_induction() {
        // The account rides into the captured row as the `account_id` salient feature, so multi-
        // aspect induction can cluster per account and scope the learned rule to that account.
        let correction = UserCorrection::CorrectLabel {
            message_id: MessageId::from("msg_1"),
            label: "suspicious".to_owned(),
        };
        let context = CorrectionContext {
            account_id: Some("work".to_owned()),
            ..ctx_with_domain("acme.test")
        };
        let feedback = build_feedback(&correction, &FeatureVector::new(), &context);
        match feedback {
            TaskFeedback::Classification(row) => {
                assert_eq!(
                    row.salient_features.get("account_id"),
                    Some(&FeatureValue::Text("work".to_owned())),
                    "the account is captured as a scoping feature"
                );
            }
            TaskFeedback::Filing(_) => panic!("expected a classification row"),
        }
    }

    #[test]
    fn an_agreeing_prior_prediction_is_positive_reinforcement() {
        let correction = UserCorrection::MarkSpam {
            message_id: MessageId::from("msg_1"),
        };
        let context = CorrectionContext {
            ai_label: Some("spam".to_owned()),
            ..CorrectionContext::default()
        };
        let feedback = build_feedback(&correction, &FeatureVector::new(), &context);
        assert_eq!(feedback.polarity(), FeedbackPolarity::Positive);
    }

    #[test]
    fn correct_label_builds_a_classification_row_with_the_chosen_label_and_prior_as_override() {
        let correction = UserCorrection::CorrectLabel {
            message_id: MessageId::from("msg_1"),
            label: "newsletters".to_owned(),
        };
        // The prior label flows in as `ai_label`, so a diverging correction is an override.
        let context = CorrectionContext {
            ai_label: Some("receipts".to_owned()),
            ..ctx_with_domain("acme.test")
        };
        let feedback = build_feedback(&correction, &FeatureVector::new(), &context);
        match feedback {
            TaskFeedback::Classification(row) => {
                assert_eq!(row.human_label, "newsletters");
                assert_eq!(row.ai_label.as_deref(), Some("receipts"));
                assert_eq!(row.polarity, FeedbackPolarity::Negative);
                assert_eq!(
                    row.salient_features.get("sender_domain"),
                    Some(&FeatureValue::Text("acme.test".to_owned()))
                );
            }
            TaskFeedback::Filing(_) => panic!("expected a classification row"),
        }
    }

    #[test]
    fn signal_marked_wrong_builds_a_negative_row_keyed_on_the_rejected_signal() {
        let correction = UserCorrection::SignalMarkedWrong {
            message_id: MessageId::from("msg_1"),
            signal_id: "auth_fail".to_owned(),
        };
        // The prior verdict was "phishing"; rejecting a reason keeps that label but records the
        // signal as the reason code, with a negative polarity (support is being removed).
        let context = CorrectionContext {
            ai_label: Some("phishing".to_owned()),
            ..ctx_with_domain("evil.test")
        };
        let feedback = build_feedback(&correction, &FeatureVector::new(), &context);
        match feedback {
            TaskFeedback::Classification(row) => {
                assert_eq!(row.human_label, "phishing");
                assert_eq!(row.human_reason_code.as_deref(), Some("auth_fail"));
                assert_eq!(row.polarity, FeedbackPolarity::Negative);
                assert_eq!(
                    row.salient_features.get("sender_domain"),
                    Some(&FeatureValue::Text("evil.test".to_owned()))
                );
            }
            TaskFeedback::Filing(_) => panic!("expected a classification row"),
        }
    }

    #[test]
    fn correct_label_to_the_same_label_is_positive_reinforcement() {
        let correction = UserCorrection::CorrectLabel {
            message_id: MessageId::from("msg_1"),
            label: "receipts".to_owned(),
        };
        let context = CorrectionContext {
            ai_label: Some("receipts".to_owned()),
            ..CorrectionContext::default()
        };
        let feedback = build_feedback(&correction, &FeatureVector::new(), &context);
        assert_eq!(feedback.polarity(), FeedbackPolarity::Positive);
    }

    #[test]
    fn filing_correction_builds_a_filing_row_keyed_on_the_folder() {
        let correction = UserCorrection::LearnFiling {
            message_id: MessageId::from("msg_1"),
            to_folder: FolderId::from("folder_receipts"),
        };
        let feedback = build_feedback(
            &correction,
            &FeatureVector::new(),
            &ctx_with_domain("s.com"),
        );
        match feedback {
            TaskFeedback::Filing(row) => {
                assert_eq!(row.human_chosen_folder, FolderId::from("folder_receipts"));
                assert_eq!(row.sender_domain.as_deref(), Some("s.com"));
                assert_eq!(row.basis.as_deref(), Some("domain"));
                assert_eq!(row.polarity, FeedbackPolarity::Negative);
            }
            TaskFeedback::Classification(_) => panic!("expected a filing row"),
        }
    }
}
