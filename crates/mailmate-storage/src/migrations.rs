//! The bundled migration runner: a shared `common` core plus a per-dialect overlay,
//! tracked in a `schema_migrations` table and applied idempotently in-process.
//!
//! Migrations are compiled into the binary with `include_str!`, so a zero-config user
//! never sees a migration step — the host creates the DB file if absent and applies them
//! on startup. Each migration runs inside a transaction (common DDL, then the dialect
//! overlay, then the bookkeeping insert), so a partial migration cannot be left behind.

use rusqlite::{params, Connection};

use mailmate_common::error::StorageError;
use mailmate_common::time::Timestamp;
use mailmate_ports::storage::Dialect;

/// One schema revision: a monotonically-increasing `version`, the shared `common` DDL,
/// and each dialect overlay it ships with.
struct Migration {
    version: i64,
    name: &'static str,
    common: &'static str,
    sqlite: &'static str,
}

/// Every embedded migration, in ascending version order. Phase 2 ships only the
/// foundational schema; later phases append `0002_…` (rule versions), `0003_…` (audit and
/// feedback), and `0004_…` (follow-ups).
const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "0001_initial",
    common: include_str!("../../../migrations/common/0001_initial.sql"),
    sqlite: include_str!("../../../migrations/sqlite/0001_initial.sql"),
}];

/// Storage engines whose migration + repository suites run in the validation matrix.
///
/// SQLite is the required, always-on leg. A server engine is appended here when its
/// backend lands; its CI leg stays opt-in / non-blocking, so the always-running
/// validation never requires an external database.
pub const ENABLED_ENGINES: &[Dialect] = &[Dialect::Sqlite];

impl Migration {
    /// The overlay SQL for `dialect`, or [`StorageError::UnsupportedEngine`] if this build
    /// ships no overlay for it.
    fn overlay(&self, dialect: Dialect) -> Result<&'static str, StorageError> {
        match dialect {
            Dialect::Sqlite => Ok(self.sqlite),
            other => Err(StorageError::UnsupportedEngine(format!(
                "no migration overlay for engine {}",
                other.as_str()
            ))),
        }
    }
}

const TRACKING_DDL: &str = "CREATE TABLE IF NOT EXISTS schema_migrations (\
     version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL)";

/// Apply every not-yet-applied embedded migration to `conn`, in version order, for the
/// given `dialect`. Idempotent: re-running applies nothing and returns an empty list.
///
/// Returns the versions applied *this* call (so a fresh DB returns `[1]` and an
/// already-current DB returns `[]`).
///
/// # Errors
/// [`StorageError::Migration`] if any DDL or bookkeeping statement fails, or
/// [`StorageError::UnsupportedEngine`] if a migration ships no overlay for `dialect`.
pub fn apply_all(conn: &mut Connection, dialect: Dialect) -> Result<Vec<i64>, StorageError> {
    conn.execute_batch(TRACKING_DDL).map_err(migration_err)?;

    let mut applied = Vec::new();
    for migration in MIGRATIONS {
        if is_applied(conn, migration.version)? {
            continue;
        }
        let overlay = migration.overlay(dialect)?;
        let tx = conn.transaction().map_err(migration_err)?;
        tx.execute_batch(migration.common).map_err(migration_err)?;
        tx.execute_batch(overlay).map_err(migration_err)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            params![
                migration.version,
                migration.name,
                Timestamp::now().to_rfc3339()
            ],
        )
        .map_err(migration_err)?;
        tx.commit().map_err(migration_err)?;
        applied.push(migration.version);
    }
    Ok(applied)
}

/// The set of already-applied migration versions, ascending — a public diagnostic used by
/// startup health checks and the migration tests.
///
/// # Errors
/// [`StorageError::Migration`] if the bookkeeping table cannot be read.
pub fn applied_versions(conn: &Connection) -> Result<Vec<i64>, StorageError> {
    let mut stmt = conn
        .prepare("SELECT version FROM schema_migrations ORDER BY version")
        .map_err(migration_err)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, i64>(0))
        .map_err(migration_err)?;
    let mut versions = Vec::new();
    for row in rows {
        versions.push(row.map_err(migration_err)?);
    }
    Ok(versions)
}

fn is_applied(conn: &Connection, version: i64) -> Result<bool, StorageError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM schema_migrations WHERE version = ?1",
            params![version],
            |row| row.get(0),
        )
        .map_err(migration_err)?;
    Ok(count > 0)
}

fn migration_err(e: rusqlite::Error) -> StorageError {
    StorageError::Migration(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        Connection::open_in_memory().expect("open in-memory sqlite")
    }

    #[test]
    fn fresh_database_applies_the_initial_migration_then_is_idempotent() {
        let mut conn = fresh();
        let first = apply_all(&mut conn, Dialect::Sqlite).unwrap();
        assert_eq!(first, vec![1], "fresh DB applies 0001");
        assert_eq!(applied_versions(&conn).unwrap(), vec![1]);

        // Re-running is a no-op (covers the "migration from prior version" idempotency).
        let second = apply_all(&mut conn, Dialect::Sqlite).unwrap();
        assert!(second.is_empty(), "re-run applies nothing");
        assert_eq!(applied_versions(&conn).unwrap(), vec![1]);
    }

    #[test]
    fn initial_migration_creates_all_foundational_tables() {
        let mut conn = fresh();
        apply_all(&mut conn, Dialect::Sqlite).unwrap();
        for table in [
            "threads",
            "messages",
            "message_features",
            "sender_profiles",
            "drafts",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table {table} should exist");
        }
    }

    #[test]
    fn unsupported_engine_has_no_overlay() {
        let migration = &MIGRATIONS[0];
        let err = migration.overlay(Dialect::Postgres).unwrap_err();
        assert!(
            matches!(err, StorageError::UnsupportedEngine(_)),
            "got {err:?}"
        );
    }
}
