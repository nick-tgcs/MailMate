//! Privacy default tests proving full body storage is disabled (the spec's "Privacy
//! default: full bodies not retained" required case), exercised through the public
//! repository API.

use mailmate_common::ids::MessageId;
use mailmate_common::message::NewMessage;
use mailmate_common::retention::RetentionLevel;
use mailmate_common::time::Timestamp;
use mailmate_ports::storage::messages::MessageRepository;
use mailmate_storage::{open_and_migrate, SqliteMessageRepository, StorageConfig};

use futures::executor::block_on;

fn message_with_body(id: &MessageId, retention: RetentionLevel) -> NewMessage {
    NewMessage {
        id: id.clone(),
        account_id: "acct_a".into(),
        folder_id: "folder_inbox".into(),
        thunderbird_message_id: "7".to_owned(),
        rfc_message_id_hash: Some("rfc".to_owned()),
        thread_id: None,
        sender_email: "s@example.com".to_owned(),
        sender_domain: "example.com".to_owned(),
        subject: "Confidential quote".to_owned(),
        received_at: Timestamp::now(),
        body_hash: Some("bodyhash".to_owned()),
        body_text: Some("CONFIDENTIAL BODY CONTENT".to_owned()),
        retention,
        created_at: Timestamp::now(),
    }
}

fn repo() -> SqliteMessageRepository {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    SqliteMessageRepository::new(backend)
}

#[test]
fn default_retention_does_not_retain_the_body() {
    let repo = repo();
    let id = MessageId::fresh();
    // A body was available, but the default Metadata retention must drop it.
    block_on(repo.insert(message_with_body(&id, RetentionLevel::Metadata))).unwrap();

    let stored = block_on(repo.get(&id)).unwrap().unwrap();
    assert!(
        !stored.body_retained,
        "default retention must not retain bodies"
    );
    assert_eq!(
        stored.body_text, None,
        "no readable body may be stored by default"
    );
    // The dedup/identity hash is still kept — a hash is not the body.
    assert_eq!(stored.body_hash.as_deref(), Some("bodyhash"));
}

#[test]
fn explicit_body_retention_keeps_the_body() {
    let repo = repo();
    let id = MessageId::fresh();
    block_on(repo.insert(message_with_body(&id, RetentionLevel::Bodies))).unwrap();

    let stored = block_on(repo.get(&id)).unwrap().unwrap();
    assert!(stored.body_retained);
    assert_eq!(
        stored.body_text.as_deref(),
        Some("CONFIDENTIAL BODY CONTENT")
    );
}

#[test]
fn summaries_retention_also_retains_the_body() {
    let repo = repo();
    let id = MessageId::fresh();
    block_on(repo.insert(message_with_body(&id, RetentionLevel::Summaries))).unwrap();
    let stored = block_on(repo.get(&id)).unwrap().unwrap();
    assert!(
        stored.body_retained,
        "summaries retention implies body retention"
    );
    assert!(stored.body_text.is_some());
}
