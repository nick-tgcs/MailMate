//! Behavioural tests for the correction use-case: a spam-axis correction is captured AND
//! teaches the Tier-2 classifier online; a filing correction is captured but trains no spam
//! model. Driven over the in-memory fakes (no storage backend, no model).

use std::sync::Arc;

use futures::executor::block_on;

use mailmate_common::correction::UserCorrection;
use mailmate_common::features::{FeatureValue, FeatureVector};
use mailmate_common::feedback::TaskFeedback;
use mailmate_common::ids::{FolderId, MessageId};
use mailmate_core::{CorrectionContext, CorrectionService};
use mailmate_test_support::fakes::{FakeLearningEngine, FakeTier2Classifier};

fn features() -> FeatureVector {
    let mut fv = FeatureVector::new();
    fv.insert("has_link", FeatureValue::Bool(true));
    fv
}

#[test]
fn spam_correction_is_captured_and_teaches_the_tier2_classifier() {
    let learning = Arc::new(FakeLearningEngine::new());
    let tier2 = Arc::new(FakeTier2Classifier::new());
    let service = CorrectionService::new(learning.clone(), tier2.clone());

    let correction = UserCorrection::MarkSpam {
        message_id: MessageId::from("msg_1"),
    };
    let context = CorrectionContext {
        sender_domain: Some("paypa1.com".to_owned()),
        ..CorrectionContext::default()
    };
    let id = block_on(service.handle_correction(correction, features(), context)).unwrap();
    assert!(id.as_str().starts_with("clsfb_"));

    // Captured into the classification feedback table…
    let captured = learning.recorded_feedback();
    assert_eq!(captured.len(), 1);
    match &captured[0] {
        TaskFeedback::Classification(row) => {
            assert_eq!(row.human_label, "spam");
            assert_eq!(
                row.salient_features.get("sender_domain"),
                Some(&FeatureValue::Text("paypa1.com".to_owned()))
            );
        }
        TaskFeedback::Filing(_) => panic!("expected a classification capture"),
    }
    // …and it also fed the Tier-2 online update with the spam-labelled example.
    let updates = tier2.observed_updates();
    assert_eq!(updates.len(), 1, "spam correction teaches Tier-2");
    assert_eq!(updates[0].label, "spam");
}

#[test]
fn filing_correction_is_captured_but_trains_no_spam_model() {
    let learning = Arc::new(FakeLearningEngine::new());
    let tier2 = Arc::new(FakeTier2Classifier::new());
    let service = CorrectionService::new(learning.clone(), tier2.clone());

    let correction = UserCorrection::LearnFiling {
        message_id: MessageId::from("msg_2"),
        to_folder: FolderId::from("folder_receipts"),
    };
    let id =
        block_on(service.handle_correction(correction, features(), CorrectionContext::default()))
            .unwrap();
    assert!(id.as_str().starts_with("filfb_"));

    let captured = learning.recorded_feedback();
    assert_eq!(captured.len(), 1);
    assert!(matches!(captured[0], TaskFeedback::Filing(_)));
    // A filing correction trains the filing target, not the spam axis.
    assert!(
        tier2.observed_updates().is_empty(),
        "filing correction must not touch the Tier-2 spam model"
    );
}
