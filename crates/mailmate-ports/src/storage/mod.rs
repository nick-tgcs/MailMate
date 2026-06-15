//! The storage seam: the engine-neutral backend contract, the SQL-dialect tag, and the
//! repository port traits the core depends on.
//!
//! The core sees ONLY these traits — never a `Connection`, `Row`, transaction handle, or
//! SQL string. Concrete backends (the bundled SQLite default; an opt-in server engine)
//! live in `mailmate-storage` as adapters that this crate never names, so the
//! no-backend-leakage law holds by construction.
//!
//! > **Placement note.** The module-layout sketch in `architecture.md` draws the
//! > repository traits *inside* `mailmate-storage`. They live here in `mailmate-ports`
//! > instead: `mailmate-storage` carries `rusqlite`, and the enforced arch-test forbids
//! > `mailmate-core` from reaching any backend — so a port the core depends on cannot sit
//! > in the same crate as the driver. The seam is unchanged; only the trait's home moves
//! > to the side of the boundary the core is allowed to touch.

use async_trait::async_trait;

use mailmate_common::error::StorageError;

pub mod audit;
pub mod conflicts;
pub mod drafts;
pub mod feedback;
pub mod messages;
pub mod proposals;
pub mod rules;
pub mod senders;
pub mod shadow_outcomes;
pub mod threads;

pub use audit::AuditRepository;
pub use conflicts::ConflictRepository;
pub use drafts::DraftRepository;
pub use feedback::FeedbackRepository;
pub use messages::MessageRepository;
pub use proposals::ProposalRepository;
pub use rules::RuleRepository;
pub use senders::SenderRepository;
pub use shadow_outcomes::ShadowOutcomeRepository;
pub use threads::ThreadRepository;

/// The SQL dialect a [`StorageBackend`] speaks. The per-dialect SQL fragments live in the
/// adapter's `dialect` module; this tag is how a backend announces which it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Dialect {
    /// Embedded SQLite — the zero-config default.
    Sqlite,
    /// PostgreSQL — opt-in server engine.
    Postgres,
    /// MySQL / MariaDB — opt-in server engine.
    MySql,
}

impl Dialect {
    /// A stable lower-case label for logging and overlay selection.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
        }
    }
}

/// The engine seam beneath the repositories: owns connections and migrations, and knows
/// its [`Dialect`].
///
/// How a backend satisfies the async contract is private: the embedded SQLite backend
/// runs synchronous `rusqlite` work behind a lock/pool; a networked backend is
/// async-native. The seam promises only that the operations are awaitable.
#[async_trait]
pub trait StorageBackend: Send + Sync {
    /// Which SQL dialect this backend speaks.
    fn dialect(&self) -> Dialect;

    /// Apply the bundled `common` + per-dialect overlay migrations, idempotently.
    ///
    /// # Errors
    /// [`StorageError::Migration`] if a migration fails to apply.
    async fn run_migrations(&self) -> Result<(), StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_is_object_safe() {
        fn takes(_: &dyn StorageBackend) {}
        let _ = takes as fn(&dyn StorageBackend);
    }

    #[test]
    fn dialect_labels_are_stable() {
        assert_eq!(Dialect::Sqlite.as_str(), "sqlite");
        assert_eq!(Dialect::Postgres.as_str(), "postgres");
        assert_eq!(Dialect::MySql.as_str(), "mysql");
    }
}
