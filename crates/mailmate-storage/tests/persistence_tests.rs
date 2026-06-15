//! End-to-end-equivalent storage flow: a file-backed database is migrated, written, fully
//! closed, then reopened in a fresh process-local connection and read back — proving the
//! schema and data persist to disk (not just to an in-memory handle). Also covers the
//! opt-in server engine being a rejected stub.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::executor::block_on;

use mailmate_common::error::StorageError;
use mailmate_common::ids::MessageId;
use mailmate_common::message::{ClassificationStatus, NewMessage};
use mailmate_common::retention::RetentionLevel;
use mailmate_common::time::Timestamp;
use mailmate_ports::storage::messages::MessageRepository;
use mailmate_ports::storage::Dialect;
use mailmate_storage::backend::{open_backend, StoragePath};
use mailmate_storage::{open_and_migrate, SqliteMessageRepository, StorageConfig};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique temp DB path (no external tempfile dependency); cleaned up by [`TempDb`].
struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mailmate-storage-test-{}-{n}.db",
            std::process::id()
        ));
        Self { path }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        // Remove the DB and its WAL/SHM sidecars; ignore if already gone.
        for suffix in ["", "-wal", "-shm"] {
            let mut p = self.path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(p));
        }
    }
}

fn sample(id: &MessageId) -> NewMessage {
    NewMessage {
        id: id.clone(),
        account_id: "acct_a".into(),
        folder_id: "folder_inbox".into(),
        thunderbird_message_id: "99".to_owned(),
        rfc_message_id_hash: None,
        thread_id: None,
        sender_email: "s@example.com".to_owned(),
        sender_domain: "example.com".to_owned(),
        subject: "Persisted".to_owned(),
        received_at: Timestamp::now(),
        body_hash: None,
        body_text: None,
        retention: RetentionLevel::Metadata,
        created_at: Timestamp::now(),
    }
}

#[test]
fn data_survives_a_full_close_and_reopen() {
    let temp = TempDb::new();
    let id = MessageId::fresh();

    // First session: migrate + insert, then drop the backend (closing the connection).
    {
        let backend = open_and_migrate(&StorageConfig::sqlite_file(&temp.path)).unwrap();
        let repo = SqliteMessageRepository::new(backend);
        block_on(repo.insert(sample(&id))).unwrap();
    }

    // Second session: reopen the same file WITHOUT re-migrating; the row is still there.
    {
        let backend = open_backend(&StorageConfig::sqlite_file(&temp.path)).unwrap();
        assert_eq!(
            backend.applied_migration_versions().unwrap(),
            vec![1, 2, 3, 4, 5],
            "schema persisted across reopen"
        );
        let repo = SqliteMessageRepository::new(backend);
        let stored = block_on(repo.get(&id)).unwrap().unwrap();
        assert_eq!(stored.subject, "Persisted");
        assert_eq!(stored.classification_status, ClassificationStatus::Pending);
    }
}

#[test]
fn server_engine_is_a_rejected_stub() {
    let err = open_backend(&StorageConfig {
        engine: Dialect::Postgres,
        path: StoragePath::InMemory,
    })
    .unwrap_err();
    assert!(
        matches!(err, StorageError::UnsupportedEngine(_)),
        "got {err:?}"
    );
}
