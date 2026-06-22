//! Concrete SQLite repository adapters implementing the `mailmate-ports` repository
//! traits over [`SqliteBackend`](crate::backend::SqliteBackend). `rusqlite` never escapes
//! these modules — every method returns domain types and [`StorageError`].
//!
//! [`StorageError`]: mailmate_common::error::StorageError

mod audit;
mod conflicts;
mod convert;
mod data_rights;
mod drafts;
mod feedback;
mod messages;
mod pipeline_items;
mod proposals;
mod reminders;
mod rules;
mod senders;
mod shadow_outcomes;
mod threads;
mod training;
mod workflows;

pub use audit::SqliteAuditRepository;
pub use conflicts::SqliteConflictRepository;
pub use data_rights::SqliteDataRightsRepository;
pub use drafts::SqliteDraftRepository;
pub use feedback::SqliteFeedbackRepository;
pub use messages::SqliteMessageRepository;
pub use pipeline_items::SqlitePipelineItemRepository;
pub use proposals::SqliteProposalRepository;
pub use reminders::SqliteReminderRepository;
pub use rules::SqliteRuleRepository;
pub use senders::SqliteSenderRepository;
pub use shadow_outcomes::SqliteShadowOutcomeRepository;
pub use threads::SqliteThreadRepository;
pub use training::{SqliteAdapterRepository, SqliteDatasetRepository, SqliteEvalRunRepository};
pub use workflows::{
    SqliteWorkflowConflictRepository, SqliteWorkflowInstanceRepository, SqliteWorkflowRepository,
    SqliteWorkflowShadowOutcomeRepository,
};
