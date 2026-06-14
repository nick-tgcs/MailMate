//! The audit repository port: the append-only timeline for cross-cutting provenance that
//! is *not* task feedback (policy blocks, provider rejections, rule-lifecycle transitions).
//! Corrections never land here — they own their per-task feedback table.

use async_trait::async_trait;

use mailmate_common::audit::{AuditEntry, AuditQuery};
use mailmate_common::error::StorageError;
use mailmate_common::ids::AuditId;

/// Append-only audit timeline.
#[async_trait]
pub trait AuditRepository: Send + Sync {
    /// Append one entry; returns its id.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn append(&self, entry: AuditEntry) -> Result<AuditId, StorageError>;

    /// Read the entries matching `query`, newest first.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn query(&self, query: AuditQuery) -> Result<Vec<AuditEntry>, StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn AuditRepository) {}
        let _ = takes as fn(&dyn AuditRepository);
    }
}
