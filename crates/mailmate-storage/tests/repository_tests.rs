//! Repository integration tests: each repository trait, driven through the public adapter
//! over a migrated in-memory backend. The queue/status flow, feature persistence, thread
//! counters, sender trust, and the always-review-required draft invariant.

use std::sync::Arc;

use futures::executor::block_on;
use serde_json::json;

use mailmate_common::draft::{DraftStatus, NewDraft};
use mailmate_common::features::{FeatureValue, FeatureVector};
use mailmate_common::ids::{DraftId, MessageId, SenderId, ThreadId};
use mailmate_common::message::{ClassificationStatus, NewMessage};
use mailmate_common::retention::RetentionLevel;
use mailmate_common::sender::{SenderProfile, TrustLevel};
use mailmate_common::thread::Thread;
use mailmate_common::time::Timestamp;
use mailmate_ports::storage::drafts::DraftRepository;
use mailmate_ports::storage::messages::MessageRepository;
use mailmate_ports::storage::senders::SenderRepository;
use mailmate_ports::storage::threads::ThreadRepository;
use mailmate_storage::{
    open_and_migrate, SqliteBackend, SqliteDraftRepository, SqliteMessageRepository,
    SqliteSenderRepository, SqliteThreadRepository, StorageConfig,
};

fn backend() -> Arc<SqliteBackend> {
    open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap()
}

fn new_message(id: &MessageId) -> NewMessage {
    NewMessage {
        id: id.clone(),
        account_id: "acct_a".into(),
        folder_id: "folder_inbox".into(),
        thunderbird_message_id: "1".to_owned(),
        rfc_message_id_hash: None,
        thread_id: None,
        sender_email: "s@example.com".to_owned(),
        sender_domain: "example.com".to_owned(),
        subject: "Hi".to_owned(),
        received_at: Timestamp::now(),
        body_hash: None,
        body_text: None,
        retention: RetentionLevel::Metadata,
        created_at: Timestamp::now(),
    }
}

#[test]
fn message_queue_status_and_features_round_trip() {
    let repo = SqliteMessageRepository::new(backend());
    let id = MessageId::fresh();
    block_on(repo.insert(new_message(&id))).unwrap();

    // Enters the queue pending; listing by status finds it.
    let pending = block_on(repo.list_by_status(ClassificationStatus::Pending)).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, id);

    // Advance through the queue.
    block_on(repo.set_classification_status(&id, ClassificationStatus::Done)).unwrap();
    assert!(block_on(repo.list_by_status(ClassificationStatus::Pending))
        .unwrap()
        .is_empty());
    assert_eq!(
        block_on(repo.get(&id))
            .unwrap()
            .unwrap()
            .classification_status,
        ClassificationStatus::Done
    );

    // Features persist and read back ordered by name with JSON-encoded values.
    let mut features = FeatureVector::new();
    features.insert("subject_len", FeatureValue::Number(2.0));
    features.insert("has_attachments", FeatureValue::Bool(false));
    block_on(repo.add_features(&id, &features)).unwrap();
    let stored = block_on(repo.get_features(&id)).unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].feature_name, "has_attachments");
    assert_eq!(stored[0].feature_value, "false");
    assert_eq!(stored[1].feature_name, "subject_len");
    assert_eq!(stored[1].feature_value, "2.0");
}

#[test]
fn thread_insert_get_and_counter_bump() {
    let repo = SqliteThreadRepository::new(backend());
    let id = ThreadId::fresh();
    block_on(repo.insert(Thread {
        id: id.clone(),
        account_id: "acct_a".into(),
        subject_root_normalized: "quote".to_owned(),
        participant_domains: vec!["example.com".to_owned(), "tgcs.com.au".to_owned()],
        message_count: 1,
        first_seen_at: Timestamp::now(),
        last_seen_at: Timestamp::now(),
        last_summary: None,
        last_summarized_at: None,
        created_at: Timestamp::now(),
    }))
    .unwrap();

    let got = block_on(repo.get(&id)).unwrap().unwrap();
    assert_eq!(got.participant_domains.len(), 2);

    block_on(repo.record_message_seen(&id, Timestamp::now())).unwrap();
    assert_eq!(block_on(repo.get(&id)).unwrap().unwrap().message_count, 2);
}

#[test]
fn sender_profile_insert_lookup_and_trust_update() {
    let repo = SqliteSenderRepository::new(backend());
    let id = SenderId::fresh();
    block_on(repo.insert(SenderProfile {
        id: id.clone(),
        email: "buyer@acme.example".to_owned(),
        domain: "acme.example".to_owned(),
        display_name: None,
        trust_level: TrustLevel::Unknown,
        last_seen_at: Timestamp::now(),
        feature_json: json!({ "threads": 2 }),
        created_at: Timestamp::now(),
        updated_at: Timestamp::now(),
    }))
    .unwrap();

    let got = block_on(repo.get_by_email("buyer@acme.example"))
        .unwrap()
        .unwrap();
    assert_eq!(got.id, id);
    assert_eq!(got.feature_json, json!({ "threads": 2 }));

    block_on(repo.set_trust_level(&id, TrustLevel::Suspicious, Timestamp::now())).unwrap();
    assert_eq!(
        block_on(repo.get_by_email("buyer@acme.example"))
            .unwrap()
            .unwrap()
            .trust_level,
        TrustLevel::Suspicious
    );
}

#[test]
fn draft_is_always_review_required_and_advances_status() {
    let repo = SqliteDraftRepository::new(backend());
    let id = DraftId::fresh();
    block_on(repo.insert(NewDraft {
        id: id.clone(),
        // No source message (a scheduled-follow-up-style draft) so this unit need not seed
        // a parent row; the FK path is covered by the message_features migration test.
        message_id: None,
        thread_id: None,
        provider_id: "mock".to_owned(),
        prompt_template_version: "v1".to_owned(),
        subject: "Re: Quote".to_owned(),
        body: "Thank you for your enquiry.".to_owned(),
        safety_flags: vec![],
        created_at: Timestamp::now(),
        updated_at: Timestamp::now(),
    }))
    .unwrap();

    let got = block_on(repo.get(&id)).unwrap().unwrap();
    assert!(got.requires_review, "every draft must be review-required");
    assert_eq!(got.status, DraftStatus::Generated);

    block_on(repo.set_status(&id, DraftStatus::Discarded, Timestamp::now())).unwrap();
    assert_eq!(
        block_on(repo.get(&id)).unwrap().unwrap().status,
        DraftStatus::Discarded
    );
}
