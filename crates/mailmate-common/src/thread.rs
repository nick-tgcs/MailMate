//! The thread entity. Thread identity is computed host-side from RFC `References` /
//! `In-Reply-To` headers (not Thunderbird's thread id); this is the persisted state.

use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, ThreadId};
use crate::time::Timestamp;

/// A persisted conversation thread.
///
/// `participant_domains` is a readable list held here as a `Vec<String>`; storage writes
/// it whole into the opaque `participant_domains` JSON column (no engine-specific JSON
/// querying). `message_count` and `last_seen_at` are the mutable counters a thread
/// accumulates as messages arrive.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Thread {
    /// Internal `thread_…` id.
    pub id: ThreadId,
    /// Owning account.
    pub account_id: AccountId,
    /// Normalized root subject (re/fwd-stripped).
    pub subject_root_normalized: String,
    /// Readable participant domains.
    pub participant_domains: Vec<String>,
    /// Number of messages seen in this thread.
    pub message_count: i64,
    /// When the thread was first seen.
    pub first_seen_at: Timestamp,
    /// When the thread was most recently seen.
    pub last_seen_at: Timestamp,
    /// Latest thread summary, populated only at `summaries`+ retention.
    pub last_summary: Option<String>,
    /// When the summary was produced.
    pub last_summarized_at: Option<Timestamp>,
    /// Insert timestamp.
    pub created_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_round_trips_through_serde() {
        let thread = Thread {
            id: ThreadId::from("thread_1"),
            account_id: AccountId::from("acct_a"),
            subject_root_normalized: "quote request".to_owned(),
            participant_domains: vec!["example.com".to_owned(), "tgcs.com.au".to_owned()],
            message_count: 3,
            first_seen_at: Timestamp::now(),
            last_seen_at: Timestamp::now(),
            last_summary: None,
            last_summarized_at: None,
            created_at: Timestamp::now(),
        };
        let json = serde_json::to_string(&thread).unwrap();
        let back: Thread = serde_json::from_str(&json).unwrap();
        assert_eq!(back, thread);
    }
}
