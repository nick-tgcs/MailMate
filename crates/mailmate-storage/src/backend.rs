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
            vec![1, 2, 3, 4]
        );
    }
}
