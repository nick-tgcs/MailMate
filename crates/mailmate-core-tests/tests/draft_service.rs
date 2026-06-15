//! Behavioural test for [`DraftService`]: it hands a bounded request to the drafter port and
//! wraps the result into a review-required draft with a fresh id — and a drafter failure
//! surfaces as a `MailMateError`. The service is driven purely through the fake drafter, so
//! the core never sees a concrete provider.

use std::sync::Arc;

use futures::executor::block_on;
use mailmate_common::error::MailMateError;
use mailmate_common::reply::{DraftedReply, ReplyDraftRequest};
use mailmate_core::DraftService;
use mailmate_test_support::fakes::FakeReplyDrafter;

#[test]
fn draft_reply_assigns_a_fresh_id_and_pins_review_required() {
    let drafter = Arc::new(FakeReplyDrafter::returning(DraftedReply {
        safety_notes: vec!["no prices, dates, or payment changes added".to_owned()],
        ..DraftedReply::new("Re: Quote", "Hi,\n\nThanks for sending this over.")
    }));
    let service = DraftService::new(drafter.clone());

    let request = ReplyDraftRequest {
        forbidden_commitments: vec!["prices".to_owned(), "dates".to_owned()],
        ..ReplyDraftRequest::new("Quote", "buyer@acme.test", "Please advise on pricing.")
    };
    let draft = block_on(service.draft_reply(request.clone())).unwrap();

    assert!(draft.draft_id.as_str().starts_with("draft_"));
    assert!(
        draft.requires_human_review,
        "a draft is never auto-sendable"
    );
    assert_eq!(draft.subject, "Re: Quote");
    assert_eq!(draft.safety_notes.len(), 1);
    // The request reached the drafter unmodified.
    assert_eq!(drafter.requests(), vec![request]);
}

#[test]
fn a_drafter_failure_propagates_as_a_mailmate_error() {
    let service = DraftService::new(Arc::new(FakeReplyDrafter::new()));
    let err = block_on(service.draft_reply(ReplyDraftRequest::new("s", "c", "e"))).unwrap_err();
    assert!(matches!(err, MailMateError::Ai(_)));
}

#[test]
fn two_drafts_get_distinct_ids() {
    let service = DraftService::new(Arc::new(FakeReplyDrafter::returning(DraftedReply::new(
        "Re: x", "y",
    ))));
    let a = block_on(service.draft_reply(ReplyDraftRequest::new("x", "c", "e"))).unwrap();
    let b = block_on(service.draft_reply(ReplyDraftRequest::new("x", "c", "e"))).unwrap();
    assert_ne!(a.draft_id, b.draft_id);
}
