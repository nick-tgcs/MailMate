//! Behavioural tests for `mailmate-core`, kept in a separate crate on purpose.
//!
//! `mailmate-core` must have ZERO dev-dependency path to any adapter or backend, so
//! its tests that need fakes live here instead of inside core. This crate depends on
//! `mailmate-core` + `mailmate-test-support`; the actual tests live under `tests/`.

/// Returns this crate's package name for smoke tests.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_available() {
        assert_eq!(super::crate_name(), "mailmate-core-tests");
    }
}
