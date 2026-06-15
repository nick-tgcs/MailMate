//! Concrete SQLite repository adapters implementing the `mailmate-ports` repository
//! traits over [`SqliteBackend`](crate::backend::SqliteBackend). `rusqlite` never escapes
//! these modules — every method returns domain types and [`StorageError`].
//!
//! [`StorageError`]: mailmate_common::error::StorageError

mod audit;
mod conflicts;
mod convert;
mod drafts;
mod feedback;
mod messages;
mod proposals;
mod rules;
mod senders;
mod shadow_outcomes;
mod threads;

pub use audit::SqliteAuditRepository;
pub use conflicts::SqliteConflictRepository;
pub use drafts::SqliteDraftRepository;
pub use feedback::SqliteFeedbackRepository;
pub use messages::SqliteMessageRepository;
pub use proposals::SqliteProposalRepository;
pub use rules::SqliteRuleRepository;
pub use senders::SqliteSenderRepository;
pub use shadow_outcomes::SqliteShadowOutcomeRepository;
pub use threads::SqliteThreadRepository;
