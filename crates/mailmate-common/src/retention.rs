//! Data-retention levels — the privacy dial that gates how much of a message persists.
//!
//! The default is [`RetentionLevel::Metadata`]: headers and computed non-body features
//! only, with **no readable body ever stored**. A user must explicitly opt up to
//! [`RetentionLevel::Bodies`] before `body_text` is retained. The levels are ordered, so
//! storage logic asks "does this level retain bodies?" rather than enumerating variants —
//! and the `messages.body_retained` column is exactly `retention.retains_body()`.

use serde::{Deserialize, Serialize};

/// How much message content MailMate may persist, in increasing order of retention.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionLevel {
    /// Headers and non-body features only. No readable body is stored. The safe default.
    #[default]
    Metadata,
    /// Additionally retain the readable `body_text`.
    Bodies,
    /// Additionally retain thread summaries (implies body retention).
    Summaries,
}

impl RetentionLevel {
    /// Whether a readable message body may be stored at this level.
    #[must_use]
    pub fn retains_body(self) -> bool {
        matches!(self, Self::Bodies | Self::Summaries)
    }

    /// Whether thread summaries may be stored at this level.
    #[must_use]
    pub fn retains_summaries(self) -> bool {
        matches!(self, Self::Summaries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_the_default_and_keeps_no_body() {
        let level = RetentionLevel::default();
        assert_eq!(level, RetentionLevel::Metadata);
        assert!(!level.retains_body(), "the default must not retain bodies");
        assert!(!level.retains_summaries());
    }

    #[test]
    fn body_and_summary_retention_follow_the_ordering() {
        assert!(!RetentionLevel::Metadata.retains_body());
        assert!(RetentionLevel::Bodies.retains_body());
        assert!(RetentionLevel::Summaries.retains_body());

        assert!(!RetentionLevel::Bodies.retains_summaries());
        assert!(RetentionLevel::Summaries.retains_summaries());

        assert!(RetentionLevel::Metadata < RetentionLevel::Bodies);
        assert!(RetentionLevel::Bodies < RetentionLevel::Summaries);
    }

    #[test]
    fn serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&RetentionLevel::Metadata).unwrap(),
            "\"metadata\""
        );
        let back: RetentionLevel = serde_json::from_str("\"bodies\"").unwrap();
        assert_eq!(back, RetentionLevel::Bodies);
    }
}
