//! The [`UserCorrection`] vocabulary: inbound signals where the user teaches the system.
//!
//! A correction is the raw material of the learning loop — it populates the per-task
//! feedback tables (Phase 7) and, for spam signals, feeds the Tier-2 classifier's online
//! update. This module owns the *vocabulary* and its deterministic projections (the label a
//! correction teaches, the filing target, a labelled training example); the persistence and
//! the rule-crystallization it drives are the learning engine's job.

use serde::{Deserialize, Serialize};

use crate::features::{FeatureVector, LabeledExample};
use crate::ids::{FolderId, MessageId};

/// Labels the spam-axis corrections teach the Tier-2 classifier.
pub const SPAM_LABEL: &str = "spam";
/// The not-spam counterpart label.
pub const HAM_LABEL: &str = "ham";

/// A correction the user makes — teaching MailMate about a message.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "correction", rename_all = "snake_case")]
pub enum UserCorrection {
    /// "This is spam."
    MarkSpam {
        /// The message corrected.
        message_id: MessageId,
    },
    /// "This is not spam."
    MarkNotSpam {
        /// The message corrected.
        message_id: MessageId,
    },
    /// "Mail like this belongs in this folder."
    LearnFiling {
        /// The message corrected.
        message_id: MessageId,
        /// The folder the user filed it into.
        to_folder: FolderId,
    },
    /// "This message's category is wrong — it should be labelled this." A one-click
    /// wrong-category correction on any classification label (not the spam axis, which
    /// [`MarkSpam`](UserCorrection::MarkSpam)/[`MarkNotSpam`](UserCorrection::MarkNotSpam)
    /// own). It teaches a classification-feedback row with an arbitrary `human_label`; it
    /// does not feed the Tier-2 spam online update (it is a category signal, not spam/ham).
    CorrectLabel {
        /// The message corrected.
        message_id: MessageId,
        /// The label the user says is correct.
        label: String,
    },
}

impl UserCorrection {
    /// The message this correction is about.
    #[must_use]
    pub fn message_id(&self) -> &MessageId {
        match self {
            Self::MarkSpam { message_id }
            | Self::MarkNotSpam { message_id }
            | Self::LearnFiling { message_id, .. }
            | Self::CorrectLabel { message_id, .. } => message_id,
        }
    }

    /// The spam-axis label this correction teaches, if it is a spam signal. A filing
    /// correction teaches a folder, not a spam label, so it returns `None`.
    #[must_use]
    pub fn spam_label(&self) -> Option<&'static str> {
        match self {
            Self::MarkSpam { .. } => Some(SPAM_LABEL),
            Self::MarkNotSpam { .. } => Some(HAM_LABEL),
            Self::LearnFiling { .. } | Self::CorrectLabel { .. } => None,
        }
    }

    /// The folder a [`LearnFiling`](UserCorrection::LearnFiling) correction points at.
    #[must_use]
    pub fn filing_target(&self) -> Option<&FolderId> {
        match self {
            Self::LearnFiling { to_folder, .. } => Some(to_folder),
            _ => None,
        }
    }

    /// Project a spam-axis correction onto a labelled training example for
    /// `Tier2Classifier::update`, given the message's features. Returns `None` for a filing
    /// correction (which trains a separate filing target, not the spam axis).
    #[must_use]
    pub fn to_labeled_example(&self, features: FeatureVector) -> Option<LabeledExample> {
        self.spam_label().map(|label| LabeledExample {
            features,
            label: label.to_owned(),
        })
    }

    /// The stable snake_case correction name (for audit rows and feedback routing).
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::MarkSpam { .. } => "mark_spam",
            Self::MarkNotSpam { .. } => "mark_not_spam",
            Self::LearnFiling { .. } => "learn_filing",
            Self::CorrectLabel { .. } => "correct_label",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spam_corrections_teach_a_spam_label_filing_does_not() {
        assert_eq!(
            UserCorrection::MarkSpam {
                message_id: MessageId::from("msg_1"),
            }
            .spam_label(),
            Some("spam")
        );
        assert_eq!(
            UserCorrection::MarkNotSpam {
                message_id: MessageId::from("msg_1"),
            }
            .spam_label(),
            Some("ham")
        );
        let filing = UserCorrection::LearnFiling {
            message_id: MessageId::from("msg_1"),
            to_folder: FolderId::from("folder_receipts"),
        };
        assert_eq!(filing.spam_label(), None);
        assert_eq!(
            filing.filing_target(),
            Some(&FolderId::from("folder_receipts"))
        );
    }

    #[test]
    fn spam_correction_projects_to_a_labelled_example() {
        let mut features = FeatureVector::new();
        features.insert("has_link", crate::features::FeatureValue::Bool(true));
        let example = UserCorrection::MarkSpam {
            message_id: MessageId::from("msg_1"),
        }
        .to_labeled_example(features.clone())
        .unwrap();
        assert_eq!(example.label, "spam");
        assert_eq!(example.features, features);

        // Filing produces no spam-axis example.
        assert!(UserCorrection::LearnFiling {
            message_id: MessageId::from("msg_1"),
            to_folder: FolderId::from("folder_x"),
        }
        .to_labeled_example(FeatureVector::new())
        .is_none());
    }

    #[test]
    fn correction_is_tagged_in_json_and_round_trips() {
        let c = UserCorrection::LearnFiling {
            message_id: MessageId::from("msg_9"),
            to_folder: FolderId::from("folder_archive"),
        };
        let value = serde_json::to_value(&c).unwrap();
        assert_eq!(value["correction"], "learn_filing");
        assert_eq!(c.name(), "learn_filing");
        assert_eq!(c.message_id(), &MessageId::from("msg_9"));
        let back: UserCorrection = serde_json::from_value(value).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn correct_label_is_a_category_signal_not_a_spam_axis_one() {
        let c = UserCorrection::CorrectLabel {
            message_id: MessageId::from("msg_3"),
            label: "newsletters".to_owned(),
        };
        // It teaches an arbitrary category label, so it has no spam-axis projection and no
        // filing target, and never fabricates a Tier-2 spam example.
        assert_eq!(c.spam_label(), None);
        assert_eq!(c.filing_target(), None);
        assert!(c.to_labeled_example(FeatureVector::new()).is_none());
        assert_eq!(c.name(), "correct_label");
        assert_eq!(c.message_id(), &MessageId::from("msg_3"));
        // Tagged + round-trips on the wire.
        let value = serde_json::to_value(&c).unwrap();
        assert_eq!(value["correction"], "correct_label");
        assert_eq!(value["label"], "newsletters");
        let back: UserCorrection = serde_json::from_value(value).unwrap();
        assert_eq!(back, c);
    }
}
