//! MailMate AI providers: the `AiProvider` adapters, the task schemas, structured
//! validation, the HTTP seam, and the provider registry.
//!
//! Edge adapter crate. The generative foundation model always runs **frozen** — there is
//! no fine-tuning path on the provider port. The four remote adapters shape requests and
//! parse responses in pure code over the [`http::HttpClient`] seam, so the default build
//! needs no network and no HTTP/TLS dependency; the concrete reqwest transport is wired at
//! the edge. `mailmate-core` depends only on the `AiProvider` port, never on this crate, so
//! no provider-specific detail can leak inward.

pub mod drafter;
pub mod http;
pub mod providers;
pub mod registry;
pub mod schemas;
pub mod tasks;
pub mod validation;

pub use drafter::TaskReplyDrafter;
pub use registry::ProviderRegistry;
pub use validation::validate_and_parse;

/// Returns this crate's package name for smoke tests.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_available() {
        assert_eq!(super::crate_name(), "mailmate-ai");
    }
}
