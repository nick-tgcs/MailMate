//! Shared, backend-free MailMate value types.
//!
//! Everything here is pure data: identifiers, the mail-client vocabulary, feature
//! vectors, the native-messaging frame envelope, secrets, the shared error taxonomy,
//! and stream aliases. No engine, runtime, database, or network crate may appear in
//! this crate's dependency tree — it is the leaf of the hexagon, depended on by the
//! ports and the core alike, and the no-backend-leakage guard enforces that.
//!
//! The set of types grows phase by phase. Phase 0 carries exactly what the
//! cross-cutting ports (`MailClient`, `Transport`, `Clock`, `SecretStore`,
//! `FeatureExtractor`, `Tier2Classifier`) reference.

pub mod draft;
pub mod error;
pub mod features;
pub mod ids;
pub mod mail;
pub mod message;
pub mod protocol;
pub mod retention;
pub mod secret;
pub mod sender;
pub mod stream;
pub mod thread;
pub mod time;

/// Returns this crate's package name for smoke tests.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_available() {
        assert_eq!(super::crate_name(), "mailmate-common");
    }
}
