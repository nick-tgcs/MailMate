//! The embedded SQLite backend: connection ownership, per-connection pragmas, the
//! `StorageBackend` impl, the `rusqlite`→`StorageError` mapping, and the `[storage]`
//! config factory.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rusqlite::{Connection, ErrorCode};

use mailmate_common::error::StorageError;
use mailmate_ports::storage::{Dialect, StorageBackend};

use crate::migrations;

/// The bundled SQLite backend.
///
/// Holds a single `Mutex<Connection>`: SQLite writes serialize through one logical
/// writer, and WAL gives readers concurrency at the file level. The `Mutex` is the
/// Phase-2 stand-in for the eventual `r2d2` pool — a backend-private detail behind the
/// seam.
#[derive(Debug)]
pub struct SqliteBackend {
    conn: Mutex<Connection>,
}

impl SqliteBackend {
    /// Open a private in-memory database (used by tests and ephemeral tooling).
    ///
    /// # Errors
    /// [`StorageError::Backend`] if the connection cannot be opened or configured.
    pub fn open_in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory().map_err(backend_err)?;
        Self::from_connection(conn)
    }

    /// Open (creating if absent) a file-backed database.
    ///
    /// # Errors
    /// [`StorageError::Backend`] if the file cannot be opened or the connection configured.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        let conn = Connection::open(path).map_err(backend_err)?;
        Self::from_connection(conn)
    }

    /// Set the per-connection pragmas and wrap the connection.
    fn from_connection(conn: Connection) -> Result<Self, StorageError> {
        // `foreign_keys=ON` (SQLite defaults it off) makes the FK constraints enforced;
        // `busy_timeout` serializes writers without spurious `SQLITE_BUSY`; WAL gives
        // readers concurrency (a no-op/`memory` on an in-memory DB, which is harmless).
        conn.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL;",
        )
        .map_err(backend_err)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Apply the bundled migrations synchronously; the worker behind `run_migrations`.
    /// Returns the versions applied this call.
    ///
    /// # Errors
    /// [`StorageError::Migration`] if a migration fails to apply.
    pub fn migrate(&self) -> Result<Vec<i64>, StorageError> {
        self.with_conn_mut(|conn| migrations::apply_all(conn, Dialect::Sqlite))
    }

    /// The already-applied migration versions — a diagnostic for startup checks/tests.
    ///
    /// # Errors
    /// [`StorageError::Migration`] if the bookkeeping table cannot be read.
    pub fn applied_migration_versions(&self) -> Result<Vec<i64>, StorageError> {
        self.with_conn(migrations::applied_versions)
    }

    /// Write a consistent snapshot of the live database to `dest` via SQLite's
    /// `VACUUM INTO`. The snapshot is a single self-contained file (no `-wal`/`-shm`
    /// sidecar), safe to take while the host keeps serving — it captures a committed,
    /// transactionally-consistent view. An existing `dest` is overwritten.
    ///
    /// # Errors
    /// [`StorageError::Backend`] if the destination directory cannot be created, a prior
    /// file cannot be removed, the path is not UTF-8, or the `VACUUM INTO` fails.
    pub fn backup_to(&self, dest: &Path) -> Result<(), StorageError> {
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    StorageError::Backend(format!("creating backup directory: {e}"))
                })?;
            }
        }
        // `VACUUM INTO` refuses to write over an existing file, so clear one first.
        if dest.exists() {
            std::fs::remove_file(dest)
                .map_err(|e| StorageError::Backend(format!("removing prior backup: {e}")))?;
        }
        let dest_str = dest
            .to_str()
            .ok_or_else(|| StorageError::Backend("backup path is not valid UTF-8".to_owned()))?;
        self.with_conn(|conn| {
            conn.execute("VACUUM INTO ?1", rusqlite::params![dest_str])
                .map(|_| ())
                .map_err(backend_err)
        })
    }

    /// Run `f` with a shared reference to the pooled connection. Crate-internal: the
    /// `Connection` never escapes to a caller outside this crate.
    pub(crate) fn with_conn<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, StorageError>,
    ) -> Result<T, StorageError> {
        let conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        f(&conn)
    }

    /// Run `f` with a mutable reference to the pooled connection (needed for
    /// transactions). Crate-internal.
    pub(crate) fn with_conn_mut<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, StorageError>,
    ) -> Result<T, StorageError> {
        let mut conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut conn)
    }
}

#[async_trait]
impl StorageBackend for SqliteBackend {
    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }

    async fn run_migrations(&self) -> Result<(), StorageError> {
        self.migrate().map(|_| ())
    }
}

/// Map a generic `rusqlite` error to [`StorageError::Backend`].
pub(crate) fn backend_err(e: rusqlite::Error) -> StorageError {
    StorageError::Backend(e.to_string())
}

/// Map a `rusqlite` error, classifying constraint violations (FK / uniqueness / NOT NULL)
/// as [`StorageError::Constraint`] and everything else as [`StorageError::Backend`].
pub(crate) fn map_rusqlite(e: rusqlite::Error) -> StorageError {
    match &e {
        rusqlite::Error::SqliteFailure(err, _) if err.code == ErrorCode::ConstraintViolation => {
            StorageError::Constraint(e.to_string())
        }
        _ => StorageError::Backend(e.to_string()),
    }
}

/// Where a SQLite database lives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoragePath {
    /// A private in-memory database (ephemeral).
    InMemory,
    /// A file-backed database, created if absent.
    File(PathBuf),
}

/// The `[storage]` configuration: which engine, and where its data lives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageConfig {
    /// The selected engine.
    pub engine: Dialect,
    /// The database location.
    pub path: StoragePath,
}

impl StorageConfig {
    /// SQLite, in memory — the test/tooling default.
    #[must_use]
    pub fn sqlite_in_memory() -> Self {
        Self {
            engine: Dialect::Sqlite,
            path: StoragePath::InMemory,
        }
    }

    /// SQLite, file-backed — the zero-config production default.
    #[must_use]
    pub fn sqlite_file(path: impl Into<PathBuf>) -> Self {
        Self {
            engine: Dialect::Sqlite,
            path: StoragePath::File(path.into()),
        }
    }
}

/// Open a backend per `config` (does not run migrations).
///
/// Only SQLite is built; a server engine returns [`StorageError::UnsupportedEngine`] — the
/// documented opt-in stub.
///
/// # Errors
/// [`StorageError::UnsupportedEngine`] for a non-SQLite engine, or [`StorageError::Backend`]
/// if the connection cannot be opened.
pub fn open_backend(config: &StorageConfig) -> Result<Arc<SqliteBackend>, StorageError> {
    match config.engine {
        Dialect::Sqlite => {
            let backend = match &config.path {
                StoragePath::InMemory => SqliteBackend::open_in_memory()?,
                StoragePath::File(path) => SqliteBackend::open(path)?,
            };
            Ok(Arc::new(backend))
        }
        other => Err(StorageError::UnsupportedEngine(format!(
            "engine {} is an opt-in server backend, not built into this binary; use sqlite",
            other.as_str()
        ))),
    }
}

/// Open a backend per `config` and apply migrations — the one-call startup path.
///
/// # Errors
/// Propagates [`open_backend`]'s errors, or [`StorageError::Migration`] on a failed
/// migration.
pub fn open_and_migrate(config: &StorageConfig) -> Result<Arc<SqliteBackend>, StorageError> {
    let backend = open_backend(config)?;
    backend.migrate()?;
    Ok(backend)
}

/// Restore the file-backed database at `config`'s path from a `backup` snapshot — an
/// **offline** operation, run while the host is not serving (the live file is replaced
/// wholesale).
///
/// It first validates that `backup` opens as a MailMate database whose schema is known and
/// not *newer* than this build (a downgrade would corrupt data), then copies it into place
/// and clears any stale `-wal`/`-shm` sidecars so SQLite re-derives them on next open. A
/// snapshot from an older schema is accepted; the next [`open_and_migrate`] brings it
/// current. The configured database need not exist yet (restore-into-fresh is supported).
///
/// # Errors
/// [`StorageError::UnsupportedEngine`] for a non-SQLite or in-memory target,
/// [`StorageError::Migration`] if the snapshot is empty or from a newer schema, or
/// [`StorageError::Backend`] on an I/O failure.
pub fn restore_database(config: &StorageConfig, backup: &Path) -> Result<(), StorageError> {
    if config.engine != Dialect::Sqlite {
        return Err(StorageError::UnsupportedEngine(
            "restore is only supported for the sqlite engine".to_owned(),
        ));
    }
    let StoragePath::File(target) = &config.path else {
        return Err(StorageError::UnsupportedEngine(
            "restore requires a file-backed database (not in-memory)".to_owned(),
        ));
    };
    if !backup.exists() {
        return Err(StorageError::Backend(format!(
            "backup file {} does not exist",
            backup.display()
        )));
    }

    // 1. Validate the snapshot: it must open and carry a known, not-newer schema head.
    let snapshot = SqliteBackend::open(backup)?;
    let versions = snapshot.applied_migration_versions()?;
    drop(snapshot);
    let head = versions.last().copied().ok_or_else(|| {
        StorageError::Migration(
            "backup has no applied migrations; it is not a MailMate database".to_owned(),
        )
    })?;
    if head > migrations::latest_version() {
        return Err(StorageError::Migration(format!(
            "backup schema (v{head}) is newer than this build (v{}); refusing to downgrade",
            migrations::latest_version()
        )));
    }

    // 2. Swap it into place and clear stale sidecars.
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StorageError::Backend(format!("creating data directory: {e}")))?;
        }
    }
    std::fs::copy(backup, target)
        .map_err(|e| StorageError::Backend(format!("copying backup into place: {e}")))?;
    for ext in ["-wal", "-shm"] {
        let sidecar = sidecar_path(target, ext);
        if sidecar.exists() {
            std::fs::remove_file(&sidecar).map_err(|e| {
                StorageError::Backend(format!("clearing stale {} sidecar: {e}", sidecar.display()))
            })?;
        }
    }
    Ok(())
}

/// The path of a SQLite sidecar (`-wal` / `-shm`) for a database file: its filename with the
/// suffix appended (e.g. `mailmate.db` → `mailmate.db-wal`).
fn sidecar_path(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_is_sqlite() {
        let backend = SqliteBackend::open_in_memory().unwrap();
        assert_eq!(backend.dialect(), Dialect::Sqlite);
    }

    #[test]
    fn foreign_keys_pragma_is_enabled() {
        let backend = SqliteBackend::open_in_memory().unwrap();
        let on: i64 = backend
            .with_conn(|conn| {
                conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))
                    .map_err(backend_err)
            })
            .unwrap();
        assert_eq!(on, 1, "FK enforcement must be ON");
    }

    #[test]
    fn open_backend_rejects_a_server_engine() {
        let config = StorageConfig {
            engine: Dialect::Postgres,
            path: StoragePath::InMemory,
        };
        let err = open_backend(&config).unwrap_err();
        assert!(
            matches!(err, StorageError::UnsupportedEngine(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn open_and_migrate_brings_a_fresh_db_current() {
        let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
        assert_eq!(
            backend.applied_migration_versions().unwrap(),
            vec![1, 2, 3, 4, 5, 6]
        );
    }

    /// Seed a tiny throwaway table so a backup/restore can be proven to carry *data*, not
    /// just schema.
    fn seed_probe(backend: &SqliteBackend, value: i64) {
        backend
            .with_conn_mut(|conn| {
                conn.execute_batch(&format!(
                    "CREATE TABLE probe (x INTEGER); INSERT INTO probe VALUES ({value});"
                ))
                .map_err(backend_err)
            })
            .unwrap();
    }

    fn read_probe(backend: &SqliteBackend) -> i64 {
        backend
            .with_conn(|conn| {
                conn.query_row("SELECT x FROM probe", [], |row| row.get(0))
                    .map_err(backend_err)
            })
            .unwrap()
    }

    #[test]
    fn backup_writes_a_consistent_self_contained_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("live.db");
        let backend = open_and_migrate(&StorageConfig::sqlite_file(&db)).unwrap();
        seed_probe(&backend, 42);

        let snapshot = dir.path().join("backups").join("snap.db");
        backend.backup_to(&snapshot).unwrap();
        assert!(
            snapshot.exists(),
            "the parent dir is created and the file written"
        );
        // No WAL sidecar is produced for the snapshot (VACUUM INTO is self-contained).
        assert!(!snapshot.with_extension("db-wal").exists());

        // The snapshot opens, is schema-current, and carries the seeded row.
        let restored = SqliteBackend::open(&snapshot).unwrap();
        assert_eq!(
            restored.applied_migration_versions().unwrap(),
            vec![1, 2, 3, 4, 5, 6]
        );
        assert_eq!(read_probe(&restored), 42);

        // Backing up again over the existing file overwrites it cleanly.
        backend.backup_to(&snapshot).unwrap();
    }

    #[test]
    fn restore_replaces_a_target_from_a_snapshot_into_a_fresh_path() {
        let dir = tempfile::tempdir().unwrap();
        let source =
            open_and_migrate(&StorageConfig::sqlite_file(dir.path().join("source.db"))).unwrap();
        seed_probe(&source, 7);
        let snapshot = dir.path().join("snap.db");
        source.backup_to(&snapshot).unwrap();

        // Restore into a path (and parent dir) that does not yet exist.
        let target = StorageConfig::sqlite_file(dir.path().join("nested").join("restored.db"));
        restore_database(&target, &snapshot).unwrap();
        let restored = open_and_migrate(&target).unwrap();
        assert_eq!(read_probe(&restored), 7);
    }

    #[test]
    fn restore_refuses_in_memory_target_and_a_missing_backup() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.db");
        // A file target but a non-existent backup → a Backend I/O error.
        let file_cfg = StorageConfig::sqlite_file(dir.path().join("t.db"));
        assert!(matches!(
            restore_database(&file_cfg, &missing).unwrap_err(),
            StorageError::Backend(_)
        ));
        // An in-memory target is rejected outright.
        assert!(matches!(
            restore_database(&StorageConfig::sqlite_in_memory(), &missing).unwrap_err(),
            StorageError::UnsupportedEngine(_)
        ));
    }

    #[test]
    fn restore_refuses_a_snapshot_from_a_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let future =
            open_and_migrate(&StorageConfig::sqlite_file(dir.path().join("future.db"))).unwrap();
        // Forge a schema head this build does not know about.
        future
            .with_conn_mut(|conn| {
                conn.execute(
                    "INSERT INTO schema_migrations (version, name, applied_at) \
                     VALUES (999, 'from_the_future', '2099-01-01T00:00:00Z')",
                    [],
                )
                .map(|_| ())
                .map_err(backend_err)
            })
            .unwrap();
        let snapshot = dir.path().join("snap.db");
        future.backup_to(&snapshot).unwrap();

        let target = StorageConfig::sqlite_file(dir.path().join("t.db"));
        let err = restore_database(&target, &snapshot).unwrap_err();
        assert!(matches!(err, StorageError::Migration(_)), "got {err:?}");
    }
}
