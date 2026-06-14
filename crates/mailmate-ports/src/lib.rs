//! Port interfaces (traits) for MailMate's adapter boundaries.
//!
//! The core depends only on these traits; every concrete engine, client, transport,
//! clock, or store is an adapter that implements one. Each is object-safe so adapters
//! can be held as `Box<dyn Port>` in a registry. The set grows phase by phase; Phase 0
//! adds the cross-cutting infrastructure ports.

pub mod clock;
pub mod feature_extractor;
pub mod mail_client;
pub mod policy_guard;
pub mod secret_store;
pub mod storage;
pub mod tier2_classifier;
pub mod transport;

/// Returns this crate's package name for smoke tests.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_available() {
        assert_eq!(super::crate_name(), "mailmate-ports");
    }
}
