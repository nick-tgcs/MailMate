//! Storage migration tests (SQLite required; engine-matrix scaffolding for opt-in server
//! legs). Covers the spec's "fresh database migration", "migration from prior version"
//! (idempotency), and "foreign-key constraints" required cases.

use mailmate_common::error::StorageError;
use mailmate_common::features::{FeatureValue, FeatureVector};
use mailmate_common::ids::MessageId;
use mailmate_common::message::NewMessage;
use mailmate_common::retention::RetentionLevel;
use mailmate_common::time::Timestamp;
use mailmate_ports::storage::messages::MessageRepository;
use mailmate_ports::storage::Dialect;
use mailmate_storage::backend::{StorageConfig, StoragePath};
use mailmate_storage::migrations::ENABLED_ENGINES;
use mailmate_storage::{open_and_migrate, SqliteMessageRepository};

use futures::executor::block_on;
use std::sync::Arc;

/// The full version set a fresh DB reaches today (0001 foundation, 0002 rule versions,
/// 0003 audit + feedback).
const CURRENT_VERSIONS: &[i64] = &[1, 2, 3];

#[test]
fn fresh_database_migrates_to_the_current_version_set() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    assert_eq!(
        backend.applied_migration_versions().unwrap(),
        CURRENT_VERSIONS
    );
}

#[test]
fn migrating_an_already_current_database_is_idempotent() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    // Re-running over the same connection applies nothing (the upgrade-from-prior path).
    let applied = backend.migrate().unwrap();
    assert!(applied.is_empty(), "no migration should re-apply");
    assert_eq!(
        backend.applied_migration_versions().unwrap(),
        CURRENT_VERSIONS
    );
}

#[test]
fn engine_matrix_scaffolding_runs_each_enabled_engine() {
    // Today only SQLite is enabled and required; a server engine appends to this list and
    // this loop runs its fresh-migration leg unchanged.
    assert!(ENABLED_ENGINES.contains(&Dialect::Sqlite));
    for &engine in ENABLED_ENGINES {
        let backend = open_and_migrate(&StorageConfig {
            engine,
            path: StoragePath::InMemory,
        })
        .unwrap();
        assert_eq!(
            backend.applied_migration_versions().unwrap(),
            CURRENT_VERSIONS,
            "engine {} should reach the current version set",
            engine.as_str()
        );
    }
}

#[test]
fn foreign_keys_are_enforced_for_orphan_features() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let repo = SqliteMessageRepository::new(Arc::clone(&backend));

    let mut features = FeatureVector::new();
    features.insert("subject_len", FeatureValue::Number(5.0));

    // No such message → message_features FK to messages(id) is violated.
    let orphan = MessageId::fresh();
    let err = block_on(repo.add_features(&orphan, &features)).unwrap_err();
    assert!(matches!(err, StorageError::Constraint(_)), "got {err:?}");

    // Control: once the parent message exists, the same insert succeeds.
    let id = MessageId::fresh();
    block_on(repo.insert(NewMessage {
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
    }))
    .unwrap();
    block_on(repo.add_features(&id, &features)).unwrap();
    assert_eq!(block_on(repo.get_features(&id)).unwrap().len(), 1);
}
