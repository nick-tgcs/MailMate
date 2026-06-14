//! The operational draft record behind a generated reply.
//!
//! Drafting Safety is enforced at the type level: [`NewDraft`] has **no**
//! `requires_review` field, so a caller cannot persist an auto-sendable draft. The
//! repository always writes `requires_review = 1`, and [`DraftRecord::requires_review`]
//! reads back as `true`. This mirrors the never-auto-send law that already keeps "send"
//! out of [`MailAction`](crate::mail::MailAction).

use serde::{Deserialize, Serialize};

use crate::ids::{DraftId, MessageId, ThreadId};
use crate::time::Timestamp;

/// Lifecycle state of a draft.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftStatus {
    /// Freshly generated, awaiting review.
    #[default]
    Generated,
    /// Edited by the user.
    Edited,
    /// Sent by the user (MailMate never sends on its own).
    Sent,
    /// Discarded by the user.
    Discarded,
}

impl DraftStatus {
    /// The stable lower-snake token stored in the `status` column.
    #[must_use]
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Generated => "generated",
            Self::Edited => "edited",
            Self::Sent => "sent",
            Self::Discarded => "discarded",
        }
    }

    /// Parse the value read back from the `status` column.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "generated" => Some(Self::Generated),
            "edited" => Some(Self::Edited),
            "sent" => Some(Self::Sent),
            "discarded" => Some(Self::Discarded),
            _ => None,
        }
    }
}

/// The fields needed to persist a newly-generated draft.
///
/// Deliberately omits `requires_review` (always `1`) and `status` (always
/// [`DraftStatus::Generated`] at insert) — neither is caller-controlled.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NewDraft {
    /// Application-generated `draft_…` id.
    pub id: DraftId,
    /// Source message replied to; `None` for a scheduled follow-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    /// Owning thread, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    /// Provider that generated the draft.
    pub provider_id: String,
    /// Versioned prompt template used.
    pub prompt_template_version: String,
    /// Draft subject.
    pub subject: String,
    /// Draft body (readable; generated locally; needed for the edit-learning signal).
    pub body: String,
    /// Draft-validator flags, held whole in the opaque `safety_flags_json` column.
    #[serde(default)]
    pub safety_flags: Vec<String>,
    /// Insert timestamp.
    pub created_at: Timestamp,
    /// Last-update timestamp.
    pub updated_at: Timestamp,
}

/// A persisted draft record as read back from storage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DraftRecord {
    /// `draft_…` id.
    pub id: DraftId,
    /// Source message replied to, if any.
    pub message_id: Option<MessageId>,
    /// Owning thread, if known.
    pub thread_id: Option<ThreadId>,
    /// Provider that generated the draft.
    pub provider_id: String,
    /// Versioned prompt template used.
    pub prompt_template_version: String,
    /// Draft subject.
    pub subject: String,
    /// Draft body.
    pub body: String,
    /// Always `true` per Drafting Safety.
    pub requires_review: bool,
    /// Draft-validator flags.
    pub safety_flags: Vec<String>,
    /// Lifecycle state.
    pub status: DraftStatus,
    /// Insert timestamp.
    pub created_at: Timestamp,
    /// Last-update timestamp.
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_status_db_strings_round_trip() {
        for status in [
            DraftStatus::Generated,
            DraftStatus::Edited,
            DraftStatus::Sent,
            DraftStatus::Discarded,
        ] {
            assert_eq!(DraftStatus::from_db_str(status.as_db_str()), Some(status));
        }
        assert_eq!(DraftStatus::from_db_str("nope"), None);
        assert_eq!(DraftStatus::default(), DraftStatus::Generated);
    }

    #[test]
    fn new_draft_has_no_requires_review_field_in_json() {
        // The safety invariant at the type level: a caller cannot set requires_review.
        let draft = NewDraft {
            id: DraftId::from("draft_1"),
            message_id: Some(MessageId::from("msg_1")),
            thread_id: None,
            provider_id: "mock".to_owned(),
            prompt_template_version: "v1".to_owned(),
            subject: "Re: Quote".to_owned(),
            body: "Thanks!".to_owned(),
            safety_flags: vec![],
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        };
        let json = serde_json::to_string(&draft).unwrap();
        assert!(!json.contains("requires_review"));
        let back: NewDraft = serde_json::from_str(&json).unwrap();
        assert_eq!(back, draft);
    }
}
