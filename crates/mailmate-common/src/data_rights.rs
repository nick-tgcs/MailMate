//! Value types for the user's data-rights operations: **erasure** ("forget this message /
//! this sender / reset all learning") and **portability** ("export everything stored about
//! me"). Backend-free — the SQLite execution lives in the storage adapter, behind the
//! `DataRightsRepository` port; these are just the request-shaped report and export documents
//! the host serialises back to the extension.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::MessageId;

/// A tally of what an erasure removed, keyed by logical table. Empty when nothing matched
/// (e.g. forgetting an unknown message): an honest "0 rows" rather than an error, so the UI
/// can say "nothing stored about that".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErasureReport {
    /// `table → rows deleted`. Only tables that actually lost rows appear.
    pub removed: BTreeMap<String, u64>,
}

impl ErasureReport {
    /// An empty report (nothing removed yet).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `n` rows removed from `table`. A zero count is ignored so the report stays a
    /// terse list of what was *actually* erased.
    pub fn record(&mut self, table: &str, n: u64) {
        if n > 0 {
            *self.removed.entry(table.to_owned()).or_insert(0) += n;
        }
    }

    /// Total rows removed across all tables.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.removed.values().copied().sum()
    }

    /// Whether anything at all was removed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// One message in a data export. The body is present **only** when retention actually kept it
/// — an export must not resurrect a body the privacy dial said not to store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedMessage {
    /// The stable message id.
    pub id: MessageId,
    /// The sender's full address.
    pub sender_email: String,
    /// The sender's domain.
    pub sender_domain: String,
    /// The subject line.
    pub subject: String,
    /// When it was received (RFC-3339).
    pub received_at: String,
    /// Whether a readable body is retained for this message.
    pub body_retained: bool,
    /// The retained body, if any (absent when `body_retained` is false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_text: Option<String>,
}

/// One classification correction in a data export — the durable learning signal, surfaced so
/// the user can see exactly what the assistant learned from them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedFeedback {
    /// The message the correction was about.
    pub message_id: MessageId,
    /// The human-chosen label.
    pub human_label: String,
    /// When the correction was made (RFC-3339).
    pub created_at: String,
}

/// One filing correction in a data export — where the user chose to file a message, the other
/// durable correction corpus alongside classification feedback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedFiling {
    /// The message the filing was about.
    pub message_id: MessageId,
    /// The folder the user chose.
    pub human_chosen_folder: String,
    /// The sender domain, if recorded (kept for clustering).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_domain: Option<String>,
    /// When the filing was made (RFC-3339).
    pub created_at: String,
}

/// One learned rule in a data export (name + authority band + lifecycle status).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedRule {
    /// The rule's stable name (its human-readable identity).
    pub stable_name: String,
    /// The hierarchy authority band (e.g. `learned_active`, `agent_shadow`).
    pub band: String,
    /// The lifecycle status (e.g. `active`, `shadow`, `disabled`).
    pub status: String,
}

/// The full "everything stored about me" document — the portability counterpart to erasure.
/// A faithful, human-readable dump of the stored messages, the corrections they fed, and the
/// rules learned from them. Bodies appear only where retention kept them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataExport {
    /// Stored messages (metadata always; body only where retained).
    pub messages: Vec<ExportedMessage>,
    /// Classification corrections (the learning signal).
    pub classification_feedback: Vec<ExportedFeedback>,
    /// Filing corrections (where the user chose to file messages).
    #[serde(default)]
    pub filing_feedback: Vec<ExportedFiling>,
    /// Learned and shadow rules.
    pub rules: Vec<ExportedRule>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_ignores_zero_and_sums_totals() {
        let mut report = ErasureReport::new();
        report.record("messages", 0);
        assert!(report.is_empty(), "a zero count records nothing");
        report.record("messages", 2);
        report.record("messages", 1);
        report.record("classification_feedback", 3);
        assert_eq!(report.total(), 6);
        assert_eq!(report.removed.get("messages"), Some(&3));
        assert!(!report.is_empty());
    }

    #[test]
    fn an_unretained_body_is_omitted_from_the_serialized_export_message() {
        let msg = ExportedMessage {
            id: MessageId::from("msg_1"),
            sender_email: "a@b.test".to_owned(),
            sender_domain: "b.test".to_owned(),
            subject: "hi".to_owned(),
            received_at: "2026-06-22T00:00:00Z".to_owned(),
            body_retained: false,
            body_text: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("body_text"), "an absent body must not appear: {json}");
    }
}
