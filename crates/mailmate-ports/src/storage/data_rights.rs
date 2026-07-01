//! The data-rights port: the user's right to **erasure** (forget a message, forget a sender,
//! reset all learning) and **portability** (export everything stored). One cohesive repository
//! because every operation crosses the same set of message-/sender-/learning-scoped tables in a
//! single transaction — the per-table repositories own *their* table, but "erase everything
//! about X" is inherently a cross-table use case, and splitting it into N delete calls would
//! lose the all-or-nothing atomicity the user is owed.

use async_trait::async_trait;

use mailmate_common::data_rights::{DataExport, ErasureReport};
use mailmate_common::error::StorageError;
use mailmate_common::ids::MessageId;

/// Erasure + portability over the stored corpus.
///
/// Each erasure runs in a single transaction: either everything about the target is gone, or
/// nothing changed. Forgetting a target that does not exist is **not** an error — it returns an
/// empty [`ErasureReport`] (the honest "nothing was stored about that").
#[async_trait]
pub trait DataRightsRepository: Send + Sync {
    /// Erase one message and everything derived from it — its body and features, the drafts and
    /// audit rows that reference it, the classification/filing corrections it produced, and any
    /// rule evidence / shadow outcomes keyed to it.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure (the transaction rolls back).
    async fn forget_message(&self, id: &MessageId) -> Result<ErasureReport, StorageError>;

    /// Erase every message from `sender_email` and everything derived from those messages, plus
    /// the sender's learned profile. Scoped to the exact address — a shared domain's *other*
    /// senders are untouched.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure (the transaction rolls back).
    async fn forget_sender(&self, sender_email: &str) -> Result<ErasureReport, StorageError>;

    /// Reset all learning: delete the correction corpus (classification/filing/follow-up
    /// feedback), the learned and shadow rules (with their versions) and their evidence,
    /// proposals, conflicts, and shadow outcomes, and the learned sender profiles. Built-in
    /// safety, human-hard, AI-suggestion, and default-fallback rules are **kept** — only what
    /// MailMate learned is forgotten. Stored messages themselves are kept (use
    /// [`forget_message`](Self::forget_message) to erase those).
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure (the transaction rolls back).
    async fn reset_learning(&self) -> Result<ErasureReport, StorageError>;

    /// Export everything stored: message metadata (and bodies *only* where retention kept them),
    /// the classification corrections, and the learned/shadow rules.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn export(&self) -> Result<DataExport, StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn DataRightsRepository) {}
        let _ = takes as fn(&dyn DataRightsRepository);
    }
}
