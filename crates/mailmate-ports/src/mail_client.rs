//! The mail-client port: command a client and observe its events.

use async_trait::async_trait;

use mailmate_common::error::MailError;
use mailmate_common::ids::{DraftId, MessageId};
use mailmate_common::mail::{DraftSpec, FetchScope, MailAction, MailEvent, MessageData};
use mailmate_common::stream::EventStream;

/// A mail client MailMate can command and observe.
///
/// Thunderbird is one adapter (over native messaging); a headless in-memory adapter
/// drives tests. The port never sends mail — [`create_draft`](MailClient::create_draft)
/// persists only, so `never_auto_send_drafts` holds at the boundary by construction.
#[async_trait]
pub trait MailClient: Send + Sync {
    /// Apply a move/tag/junk/read/flag action to a message.
    async fn apply(&self, action: MailAction) -> Result<(), MailError>;

    /// Persist a draft (never sends) and return its operational id.
    async fn create_draft(&self, spec: DraftSpec) -> Result<DraftId, MailError>;

    /// Fetch a message, bounded by `scope` (and the active retention level).
    async fn fetch(&self, id: MessageId, scope: FetchScope) -> Result<MessageData, MailError>;

    /// A stream of new-mail and user-action events from the client.
    fn events(&self) -> EventStream<MailEvent>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        // Proves `dyn MailClient` is a valid type, which the adapter registry relies on.
        fn takes(_: &dyn MailClient) {}
        let _ = takes as fn(&dyn MailClient);
    }
}
