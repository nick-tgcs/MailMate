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

pub mod action;
pub mod actor;
pub mod adapter;
pub mod ai;
pub mod audit;
pub mod classification;
pub mod conflict;
pub mod correction;
pub mod curator;
pub mod draft;
pub mod error;
pub mod evidence;
pub mod features;
pub mod feedback;
pub mod hashing;
pub mod ids;
pub mod mail;
pub mod message;
pub mod outcome;
pub mod planning;
pub mod policy;
pub mod proposal;
pub mod protocol;
pub mod reply;
pub mod retention;
pub mod rules;
pub mod secret;
pub mod sender;
pub mod shadow;
pub mod stream;
pub mod task;
pub mod thread;
pub mod time;
pub mod training;

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
