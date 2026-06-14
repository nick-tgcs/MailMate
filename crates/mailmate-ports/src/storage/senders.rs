//! The sender repository port: persist sender profiles and update their trust level.

use async_trait::async_trait;

use mailmate_common::error::StorageError;
use mailmate_common::ids::SenderId;
use mailmate_common::sender::{SenderProfile, TrustLevel};
use mailmate_common::time::Timestamp;

/// Persistence for sender profiles.
#[async_trait]
pub trait SenderRepository: Send + Sync {
    /// Persist a new sender profile.
    ///
    /// # Errors
    /// [`StorageError`] on a constraint violation or backend failure.
    async fn insert(&self, profile: SenderProfile) -> Result<(), StorageError>;

    /// Fetch the most recently-updated profile for an email, or `None` if absent.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn get_by_email(&self, email: &str) -> Result<Option<SenderProfile>, StorageError>;

    /// Update a sender's trust level and `updated_at`.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn set_trust_level(
        &self,
        id: &SenderId,
        trust: TrustLevel,
        updated_at: Timestamp,
    ) -> Result<(), StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn SenderRepository) {}
        let _ = takes as fn(&dyn SenderRepository);
    }
}
