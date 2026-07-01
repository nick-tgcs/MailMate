//! Durable **remind-me / snooze** value types (Phase 9): a notify-only timer over a message or
//! thread. Unlike the sales-cadence follow-up workflow — which drafts a reply when a step fires —
//! a reminder only *nudges*: when it comes due the host emits one notification and the reminder is
//! done. Snooze is just a reschedule of `due_at`; "send-later" is a reminder whose note points at
//! a saved draft. Backend-free: persistence lives behind the `ReminderRepository` port.

use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, MessageId, ReminderId, ThreadId};
use crate::time::Timestamp;

/// Where a reminder is in its (single-shot) lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ReminderStatus {
    /// Armed and ticking — selected by the drain once `due_at` passes.
    Pending,
    /// Already nudged — terminal, never fires again (idempotent drains rely on this).
    Fired,
    /// The user dismissed it before it fired — terminal.
    Cancelled,
}

impl ReminderStatus {
    /// The DB label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Fired => "fired",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse a DB label back to a status.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "fired" => Some(Self::Fired),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// Whether a reminder in this status is still selectable by the drain (only `Pending` is).
    #[must_use]
    pub fn is_pending(self) -> bool {
        matches!(self, Self::Pending)
    }
}

/// A request to arm a new reminder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewReminder {
    /// Application-generated id (known before insert).
    pub id: ReminderId,
    /// The message this reminder is about, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    /// The thread this reminder is about, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    /// The owning account, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<AccountId>,
    /// A short human label (e.g. the subject) shown in the nudge.
    pub title: String,
    /// An optional free-text note (e.g. "reply with the quote", or a saved-draft pointer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When the nudge should fire.
    pub due_at: Timestamp,
    /// Insert timestamp.
    pub created_at: Timestamp,
}

/// A persisted reminder, read back from the store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reminder {
    /// The reminder id (`rem_…`).
    pub id: ReminderId,
    /// The message it is about, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    /// The thread it is about, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    /// The owning account, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<AccountId>,
    /// The human label shown in the nudge.
    pub title: String,
    /// The optional free-text note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When it fires.
    pub due_at: Timestamp,
    /// Lifecycle status.
    pub status: ReminderStatus,
    /// When it was armed.
    pub created_at: Timestamp,
    /// When it actually fired, if it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired_at: Option<Timestamp>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trips_through_its_db_label() {
        for status in [
            ReminderStatus::Pending,
            ReminderStatus::Fired,
            ReminderStatus::Cancelled,
        ] {
            assert_eq!(ReminderStatus::from_db_str(status.as_str()), Some(status));
        }
        assert_eq!(ReminderStatus::from_db_str("bogus"), None);
        assert!(ReminderStatus::Pending.is_pending());
        assert!(!ReminderStatus::Fired.is_pending());
    }
}
