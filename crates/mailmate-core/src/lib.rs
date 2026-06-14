//! MailMate core: domain entities and use-cases.
//!
//! The product lives here. It depends ONLY on `mailmate-ports` (and, once use-cases
//! land, `mailmate-common`) — never on an adapter or a backend crate. That constraint
//! is enforced from the outside by `mailmate-arch-test` and the no-backend-leakage CI
//! guard, and core's behavioural tests live in `mailmate-core-tests` so `mailmate-core`
//! keeps zero dev-dependency path to any backend.

use std::sync::Arc;

use mailmate_ports::clock::Clock;
use mailmate_ports::feature_extractor::FeatureExtractor;
use mailmate_ports::mail_client::MailClient;
use mailmate_ports::secret_store::SecretStore;
use mailmate_ports::tier2_classifier::Tier2Classifier;
use mailmate_ports::transport::Transport;

/// The set of adapters the core's use-cases run against — the dependency-injection
/// seam.
///
/// The core only ever holds *ports* here (`Arc<dyn Port>`); concrete adapters are
/// injected at the edge in production and as fakes in tests, and the core never names
/// either. Cloning is cheap (reference-counted) so use-cases can hold their own handle.
#[derive(Clone)]
pub struct Ports {
    /// The mail client (Thunderbird in production; a fake in tests).
    pub mail_client: Arc<dyn MailClient>,
    /// The IPC transport.
    pub transport: Arc<dyn Transport>,
    /// The clock (system wall-clock in production; a fake clock in tests).
    pub clock: Arc<dyn Clock>,
    /// The secret store.
    pub secret_store: Arc<dyn SecretStore>,
    /// The deterministic feature extractor.
    pub feature_extractor: Arc<dyn FeatureExtractor>,
    /// The Tier-2 classifier.
    pub tier2: Arc<dyn Tier2Classifier>,
}

impl Ports {
    /// Assemble a port bundle from already-constructed adapters.
    #[must_use]
    pub fn new(
        mail_client: Arc<dyn MailClient>,
        transport: Arc<dyn Transport>,
        clock: Arc<dyn Clock>,
        secret_store: Arc<dyn SecretStore>,
        feature_extractor: Arc<dyn FeatureExtractor>,
        tier2: Arc<dyn Tier2Classifier>,
    ) -> Self {
        Self {
            mail_client,
            transport,
            clock,
            secret_store,
            feature_extractor,
            tier2,
        }
    }
}

/// Returns this crate's package name for smoke tests.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_available() {
        assert_eq!(super::crate_name(), "mailmate-core");
    }
}
