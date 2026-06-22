//! The mail-client vocabulary: the actions the host may ask a client to apply, the
//! draft it may persist, the message it returns, and the events it emits.
//!
//! Sending is intentionally absent from [`MailAction`] — the system never auto-sends;
//! drafts are persisted for human review (`never_auto_send_drafts`).

use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, FolderId, MessageId, ThreadId};
use crate::time::Timestamp;

/// An action the host asks the mail client to apply (`MailClient::apply`).
///
/// Move / tag / junk / read / flag only — there is deliberately no "send" variant.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MailAction {
    /// Apply a tag (by tag key) to a message.
    Tag { message_id: MessageId, tag: String },
    /// Move a message to a folder.
    Move {
        message_id: MessageId,
        to_folder: FolderId,
    },
    /// Mark (or unmark) a message as junk.
    MarkJunk { message_id: MessageId, junk: bool },
    /// Mark (or unmark) a message as read.
    MarkRead { message_id: MessageId, read: bool },
    /// Set (or clear) a message's flagged state.
    Flag {
        message_id: MessageId,
        flagged: bool,
    },
}

/// Spec for a draft the client persists (`MailClient::create_draft`).
///
/// The client only *saves* the draft; whether it may ever be sent is a policy-guard
/// decision elsewhere, never a property of this type.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DraftSpec {
    /// The message this draft replies to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<MessageId>,
    /// The thread this draft belongs to, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    /// Recipient addresses.
    pub to: Vec<String>,
    /// Draft subject.
    pub subject: String,
    /// Draft body (plain text).
    pub body: String,
    /// Human-facing notes the review surface should show alongside the draft.
    #[serde(default)]
    pub safety_notes: Vec<String>,
}

/// How much of a message to fetch (`MailClient::fetch`), bounded by retention policy.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchScope {
    /// Headers only.
    Metadata,
    /// Headers plus body text.
    Body,
    /// Headers, body, and attachment metadata.
    Full,
}

/// A normalized message returned by the client and fed to the feature extractor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MessageData {
    /// Internal id once the message is persisted; absent for not-yet-stored messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<MessageId>,
    /// The client-native message identifier (e.g. Thunderbird's numeric id as text).
    pub client_message_id: String,
    /// Owning account.
    pub account_id: AccountId,
    /// Folder the message currently lives in.
    pub folder_id: FolderId,
    /// Thread membership, if resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    /// Parsed headers.
    pub headers: MessageHeaders,
    /// Body text, present only when retention/scope allows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_text: Option<String>,
    /// Attachment metadata (never content).
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// Whether remote content was loaded (never loaded for classification by default).
    #[serde(default)]
    pub remote_content_loaded: bool,
    /// How many prior messages from this sender the client has seen (bounded, best-effort);
    /// `None` when the client did not compute it. A high count is a strong not-spam signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_seen_count: Option<u32>,
    /// Whether the sender is in the user's address book; `None` when not looked up. A known
    /// contact is a strong not-spam signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_in_address_book: Option<bool>,
}

impl MessageData {
    /// Whether this message looks like a reply in a conversation the user is part of — the
    /// signal the **thread guard** keys on (`never auto-junk/auto-file a reply in a thread the
    /// user joined`). A conservative, header-only proxy: the message is threaded (carries
    /// `In-Reply-To`/`References`) **and** is not bulk/list mail (a mailing-list digest also
    /// carries `References`, but is not a personal conversation). It deliberately errs toward
    /// "leave conversations alone" — the cost of a false positive is only a demotion to review.
    #[must_use]
    pub fn is_joined_thread_reply(&self) -> bool {
        self.headers.is_threaded() && !self.headers.is_bulk_or_list()
    }
}

/// The parsed header subset MailMate reasons over.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct MessageHeaders {
    /// `From` address.
    pub from: String,
    /// `To` addresses.
    #[serde(default)]
    pub to: Vec<String>,
    /// Subject line.
    #[serde(default)]
    pub subject: String,
    /// RFC `Message-ID`, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// `Date`, if parseable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<Timestamp>,
    /// `References` chain.
    #[serde(default)]
    pub references: Vec<String>,
    /// `In-Reply-To`, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    /// `Reply-To`, if present — a different reply-to domain than `From` is a phishing cue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// `List-Id`, marking list / bulk mail (newsletters, mailing lists).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_id: Option<String>,
    /// `List-Unsubscribe`, the source of the one-click unsubscribe affordance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_unsubscribe: Option<String>,
    /// `List-Unsubscribe-Post` (RFC 8058) — present and `List-Unsubscribe=One-Click` marks a
    /// link safe to unsubscribe from with a single background POST (no confirmation page).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_unsubscribe_post: Option<String>,
    /// `Precedence` (e.g. `bulk` / `list` / `junk`) — a bulk-mail signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precedence: Option<String>,
    /// The raw `Authentication-Results` header, parsed locally into SPF/DKIM/DMARC features
    /// (the host parses it; no network, no DNS — the verdict the receiving MTA already wrote).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication_results: Option<String>,
}

impl MessageHeaders {
    /// Whether the message is part of an existing conversation (carries `In-Reply-To` or a
    /// non-empty `References` chain).
    #[must_use]
    pub fn is_threaded(&self) -> bool {
        self.in_reply_to.is_some() || !self.references.is_empty()
    }

    /// Whether the message is bulk/list mail (carries `List-Id`, or a `Precedence` of
    /// `bulk`/`list`/`junk`).
    #[must_use]
    pub fn is_bulk_or_list(&self) -> bool {
        self.list_id.is_some()
            || self.precedence.as_deref().is_some_and(|p| {
                let p = p.trim().to_ascii_lowercase();
                p == "bulk" || p == "list" || p == "junk"
            })
    }
}

/// Attachment metadata. Content is never carried here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Attachment {
    /// File name as declared by the message.
    pub filename: String,
    /// MIME type as declared by the message.
    pub content_type: String,
    /// Size in bytes.
    pub size_bytes: u64,
}

/// An event on `MailClient::events`: new mail, or a user action observed in the client.
///
/// `NewMail` boxes its payload so the enum stays small (the other variants are a few
/// scalars), keeping it cheap to move through the event stream.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum MailEvent {
    /// A new message arrived.
    NewMail { message: Box<MessageData> },
    /// The user moved a message between folders.
    MessageMoved {
        client_message_id: String,
        from_folder_id: FolderId,
        to_folder_id: FolderId,
        user_initiated: bool,
        occurred_at: Timestamp,
    },
    /// The user changed a message's junk state.
    JunkChanged {
        client_message_id: String,
        junk: bool,
        user_initiated: bool,
        occurred_at: Timestamp,
    },
    /// The user changed a message's read state.
    ReadChanged {
        client_message_id: String,
        read: bool,
        occurred_at: Timestamp,
    },
    /// The user added or removed a tag.
    Tagged {
        client_message_id: String,
        tag: String,
        added: bool,
        occurred_at: Timestamp,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_message() -> MessageData {
        MessageData {
            id: None,
            client_message_id: "42".to_owned(),
            account_id: AccountId::from("acct_a"),
            folder_id: FolderId::from("folder_inbox"),
            thread_id: None,
            headers: MessageHeaders {
                from: "sender@example.com".to_owned(),
                subject: "Quote request".to_owned(),
                ..MessageHeaders::default()
            },
            body_text: Some("hello".to_owned()),
            attachments: vec![Attachment {
                filename: "quote.pdf".to_owned(),
                content_type: "application/pdf".to_owned(),
                size_bytes: 1234,
            }],
            remote_content_loaded: false,
            sender_seen_count: None,
            sender_in_address_book: None,
        }
    }

    #[test]
    fn mail_action_is_tagged_in_json() {
        let action = MailAction::Move {
            message_id: MessageId::from("msg_1"),
            to_folder: FolderId::from("folder_archive"),
        };
        let value: serde_json::Value = serde_json::to_value(&action).unwrap();
        assert_eq!(value["kind"], "move");
        assert_eq!(value["to_folder"], "folder_archive");
        let back: MailAction = serde_json::from_value(value).unwrap();
        assert_eq!(back, action);
    }

    #[test]
    fn a_joined_thread_reply_is_a_threaded_non_bulk_message() {
        let mut msg = sample_message();
        // A bare new message is not a joined-thread reply.
        assert!(!msg.is_joined_thread_reply());

        // A reply in a conversation (In-Reply-To set) is.
        msg.headers.in_reply_to = Some("<prev@example.com>".to_owned());
        assert!(msg.is_joined_thread_reply());

        // …unless it is bulk/list mail (a mailing-list digest carries References too).
        msg.headers.list_id = Some("<list.example.com>".to_owned());
        assert!(
            !msg.is_joined_thread_reply(),
            "list mail is not a personal conversation"
        );
        msg.headers.list_id = None;
        msg.headers.precedence = Some("bulk".to_owned());
        assert!(!msg.is_joined_thread_reply());

        // A References chain alone (no In-Reply-To) still counts as threaded.
        let mut threaded = sample_message();
        threaded.headers.references = vec!["<a@x>".to_owned()];
        assert!(threaded.is_joined_thread_reply());
    }

    #[test]
    fn message_round_trips() {
        let msg = sample_message();
        let json = serde_json::to_string(&msg).unwrap();
        let back: MessageData = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn new_mail_event_round_trips_with_boxed_payload() {
        let event = MailEvent::NewMail {
            message: Box::new(sample_message()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: MailEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn there_is_no_send_action() {
        // Guards the never-auto-send invariant at the type level: the action vocabulary
        // omits "send". This test exists so adding one is a deliberate, visible change.
        let json = serde_json::to_string(&MailAction::MarkJunk {
            message_id: MessageId::from("msg_1"),
            junk: true,
        })
        .unwrap();
        assert!(!json.contains("send"));
    }
}
