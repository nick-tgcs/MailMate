//! MailMate storage adapters: the bundled SQLite backend, the migration runner, and the
//! repository implementations over the `StorageBackend` seam.
//!
//! This crate is an **edge adapter**. It is the only place `rusqlite` (and, later, any
//! server driver) appears; `mailmate-core` never depends on it, so the no-backend-leakage
//! law holds. The core sees only the repository *port traits* from `mailmate-ports`; the
//! `Sqlite*Repository` types here implement them, and a `rusqlite::Connection` never
//! escapes a repository method.
//!
//! Async model (Phase 2): the embedded backend holds a single `Mutex<Connection>` and
//! does its `rusqlite` work *inline* inside the async methods — there is no `.await`
//! across the lock, so the futures resolve immediately and tests drive them with
//! `futures::executor::block_on`. The seam only promises async; this is one valid way to
//! satisfy it. An `r2d2` pool + `spawn_blocking` is a backend-private optimization that
//! can replace the `Mutex` later without touching the port traits or the core.

pub mod backend;
pub mod dialect;
pub mod migrations;
pub mod repositories;

pub use backend::{open_and_migrate, open_backend, SqliteBackend, StorageConfig, StoragePath};
pub use repositories::{
    SqliteAuditRepository, SqliteConflictRepository, SqliteDraftRepository,
    SqliteFeedbackRepository, SqliteMessageRepository, SqliteProposalRepository,
    SqliteRuleRepository, SqliteSenderRepository, SqliteShadowOutcomeRepository,
    SqliteThreadRepository,
};

/// Returns this crate's package name for smoke tests.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_available() {
        assert_eq!(super::crate_name(), "mailmate-storage");
    }
}
