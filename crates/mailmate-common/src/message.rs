//! The persisted message entity, its background-queue status, and its stored features.
//!
//! [`MessageData`](crate::mail::MessageData) is the *wire* message the mail client hands
//! over; [`StoredMessage`] is the *persisted* record after intake. They are deliberately
//! distinct: storage gates the readable body behind the active
//! [`RetentionLevel`](crate::retention::RetentionLevel), so the stored shape is not the
//! fetched shape. [`NewMessage`] is the insert input; the repository — not the caller —
//! decides whether `body_text` is actually written.

use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, FolderId, MessageFeatureId, MessageId, ThreadId};
use crate::retention::RetentionLevel;
use crate::time::Timestamp;

/// Background-classification queue state for a message.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationStatus {
    /// Awaiting classification.
    #[default]
    Pending,
    /// Being classified now.
    Processing,
    /// Classification finished.
    Done,
    /// Classification failed (to be retried or surfaced).
    Failed,
}

impl ClassificationStatus {
    /// The stable lower-snake token stored in the `classification_status` column.
    #[must_use]
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    /// Parse the value read back from the `classification_status` column.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "processing" => Some(Self::Processing),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// The fields needed to persist a newly-intaken message.
///
/// `body_text` is the body as fetched; whether it is actually written is decided at
/// insert time from `retention` — at [`RetentionLevel::Metadata`] (the default) the body
/// is dropped and only `body_hash` (identity/dedup) may remain. `classification_status`
/// is not part of intake: a new message always enters the queue as
/// [`ClassificationStatus::Pending`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NewMessage {
    /// Application-generated id (known before insert).
    pub id: MessageId,
    /// Owning account.
    pub account_id: AccountId,
    /// Folder the message currently lives in.
    pub folder_id: FolderId,
    /// Adapter-provided id (not stable across reindex).
    pub thunderbird_message_id: String,
    /// Hash of the RFC `Message-ID` (stable identity/dedup), if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rfc_message_id_hash: Option<String>,
    /// Internal thread membership, if resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    /// Readable sender address.
    pub sender_email: String,
    /// Readable sender domain.
    pub sender_domain: String,
    /// Readable subject.
    pub subject: String,
    /// When the message was received.
    pub received_at: Timestamp,
    /// Hash of the canonical body (identity/dedup), retained regardless of body text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_hash: Option<String>,
    /// Candidate readable body; persisted only when `retention.retains_body()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_text: Option<String>,
    /// The retention level in force, gating body persistence.
    pub retention: RetentionLevel,
    /// Insert timestamp.
    pub created_at: Timestamp,
}

/// A persisted message record as read back from storage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredMessage {
    /// Internal `msg_…` id.
    pub id: MessageId,
    /// Owning account.
    pub account_id: AccountId,
    /// Folder the message lives in.
    pub folder_id: FolderId,
    /// Adapter-provided id.
    pub thunderbird_message_id: String,
    /// Hash of the RFC `Message-ID`, if known.
    pub rfc_message_id_hash: Option<String>,
    /// Internal thread membership, if resolved.
    pub thread_id: Option<ThreadId>,
    /// Readable sender address.
    pub sender_email: String,
    /// Readable sender domain.
    pub sender_domain: String,
    /// Readable subject.
    pub subject: String,
    /// When the message was received.
    pub received_at: Timestamp,
    /// Background-queue state.
    pub classification_status: ClassificationStatus,
    /// Hash of the canonical body, if known.
    pub body_hash: Option<String>,
    /// Whether a readable body was retained (mirrors `body_retained` 0/1).
    pub body_retained: bool,
    /// The readable body, present only when `body_retained` is true.
    pub body_text: Option<String>,
    /// Insert timestamp.
    pub created_at: Timestamp,
}

/// A stored non-body feature row (`message_features`).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredFeature {
    /// Surrogate `mf_…` id.
    pub id: MessageFeatureId,
    /// Owning message.
    pub message_id: MessageId,
    /// Feature name (e.g. `subject_len`, `has_attachments`).
    pub feature_name: String,
    /// Feature value as a JSON scalar/object string.
    pub feature_value: String,
    /// Insert timestamp.
    pub created_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_status_db_strings_round_trip() {
        for status in [
            ClassificationStatus::Pending,
            ClassificationStatus::Processing,
            ClassificationStatus::Done,
            ClassificationStatus::Failed,
        ] {
            assert_eq!(
                ClassificationStatus::from_db_str(status.as_db_str()),
                Some(status)
            );
        }
        assert_eq!(ClassificationStatus::from_db_str("bogus"), None);
        assert_eq!(
            ClassificationStatus::default(),
            ClassificationStatus::Pending
        );
    }

    #[test]
    fn stored_message_round_trips_through_serde() {
        let msg = StoredMessage {
            id: MessageId::from("msg_1"),
            account_id: AccountId::from("acct_a"),
            folder_id: FolderId::from("folder_inbox"),
            thunderbird_message_id: "42".to_owned(),
            rfc_message_id_hash: Some("abc".to_owned()),
            thread_id: Some(ThreadId::from("thread_x")),
            sender_email: "s@example.com".to_owned(),
            sender_domain: "example.com".to_owned(),
            subject: "Quote".to_owned(),
            received_at: Timestamp::now(),
            classification_status: ClassificationStatus::Pending,
            body_hash: None,
            body_retained: false,
            body_text: None,
            created_at: Timestamp::now(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: StoredMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }
}
