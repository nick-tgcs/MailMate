//! Concrete SQLite repository adapters implementing the `mailmate-ports` repository
//! traits over [`SqliteBackend`](crate::backend::SqliteBackend). `rusqlite` never escapes
//! these modules — every method returns domain types and [`StorageError`].
//!
//! [`StorageError`]: mailmate_common::error::StorageError

mod convert;
mod drafts;
mod messages;
mod senders;
mod threads;

pub use drafts::SqliteDraftRepository;
pub use messages::SqliteMessageRepository;
pub use senders::SqliteSenderRepository;
pub use threads::SqliteThreadRepository;
