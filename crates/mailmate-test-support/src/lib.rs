//! In-memory fakes and deterministic fixtures implementing the Phase-0 ports.
//!
//! A normal library crate (not dev-only): other crates pull it in under their own
//! `[dev-dependencies]`. Fakes are hand-written (no mocking framework), record their
//! calls for assertions, and behave deterministically so every contract test replays
//! identically. The crate depends only on pure crates (`-common`, `-ports`), so it
//! never drags a backend into anyone's tree.

pub mod fakes;
pub mod fixtures;

/// Returns this crate's package name for smoke tests.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_available() {
        assert_eq!(super::crate_name(), "mailmate-test-support");
    }
}
