//! The core `PlanningService` use-case, proven over the in-memory fakes: it sequences
//! classify → plan → guard and routes each step through the boxed `dyn Port`. This keeps
//! `mailmate-core` free of any backend dev-dependency (the real cascade/planner/guard are
//! exercised end-to-end in `mailmate-planner`'s tests).

use std::sync::Arc;

use futures::executor::block_on;

use mailmate_common::action::ProposedAction;
use mailmate_common::classification::{Classification, ClassificationProvenance, Priority};
use mailmate_common::ids::{AccountId, DecisionId, FolderId, MessageId};
use mailmate_common::mail::{MessageData, MessageHeaders};
use mailmate_core::PlanningService;
use mailmate_test_support::fakes::{
    FakeActionPlanner, FakeClassificationEngine, FakePolicyGuard, StubFeatureExtractor,
};

fn message() -> MessageData {
    MessageData {
        id: Some(MessageId::from("msg_1")),
        client_message_id: "1".to_owned(),
        account_id: AccountId::from("acct_a"),
        folder_id: FolderId::from("folder_inbox"),
        thread_id: None,
        headers: MessageHeaders {
            from: "sender@example.com".to_owned(),
            subject: "Quote request".to_owned(),
            ..MessageHeaders::default()
        },
        body_text: None,
        attachments: vec![],
        remote_content_loaded: false,
        sender_seen_count: None,
        sender_in_address_book: None,
    }
}

fn classification() -> Classification {
    Classification {
        decision_id: DecisionId::from("dec_seed"),
        labels: vec!["general".to_owned()],
        spam_score: 0.1,
        phishing_score: 0.0,
        priority: Priority::Normal,
        needs_review: false,
        confidence: 0.0,
        salient_signals: Vec::new(),
        safety_findings: Vec::new(),
        provenance: ClassificationProvenance::tier2("fake-v1", false),
    }
}

#[test]
fn planning_service_runs_classify_plan_guard_over_the_ports() {
    let classifier = Arc::new(FakeClassificationEngine::returning(classification()));
    let planner = Arc::new(FakeActionPlanner::returning(vec![ProposedAction::Tag {
        message_id: MessageId::from("msg_1"),
        tag: "quote".to_owned(),
    }]));
    let service = PlanningService::new(
        Arc::new(StubFeatureExtractor),
        classifier.clone(),
        planner.clone(),
        Arc::new(FakePolicyGuard::new()),
    );

    let outcome = block_on(service.handle_new_mail(message())).unwrap();

    // P1 ran (the message reached the classifier) and its verdict is carried out.
    assert_eq!(classifier.classified(), vec![MessageId::from("msg_1")]);
    assert_eq!(outcome.classification.labels, vec!["general".to_owned()]);

    // P2 ran (the planner saw the classification) and the guard allowed the safe tag.
    assert_eq!(planner.planned().len(), 1);
    assert_eq!(outcome.guarded_plan.allowed_actions.len(), 1);
    assert!(outcome.guarded_plan.blocked_actions.is_empty());

    // The whole chain shares one decision identity (minted at the classify step).
    assert_eq!(
        outcome.classification.decision_id,
        outcome.guarded_plan.decision_id
    );
}

#[test]
fn a_prohibited_proposal_is_blocked_by_the_guard_step() {
    // Even if a (hypothetical) planner proposed a prohibited act, the guard step blocks it —
    // proving the use-case really routes the plan through the guard rather than applying it.
    let planner = Arc::new(FakeActionPlanner::returning(vec![
        ProposedAction::SendDraft {
            draft_id: mailmate_common::ids::DraftId::from("draft_1"),
        },
    ]));
    let service = PlanningService::new(
        Arc::new(StubFeatureExtractor),
        Arc::new(FakeClassificationEngine::returning(classification())),
        planner,
        Arc::new(FakePolicyGuard::new()),
    );

    let outcome = block_on(service.handle_new_mail(message())).unwrap();
    assert!(outcome.guarded_plan.allowed_actions.is_empty());
    assert_eq!(outcome.guarded_plan.blocked_actions.len(), 1);
}
