//! Deterministic data builders (no I/O at call time), shared across contract tests.

use mailmate_common::ids::{AccountId, FolderId, MessageId};
use mailmate_common::mail::{Attachment, MessageData, MessageHeaders};

/// A representative inbound message with a stable id, fetchable from a `FakeMailClient`.
#[must_use]
pub fn sample_message() -> MessageData {
    MessageData {
        id: Some(MessageId::from("msg_sample")),
        client_message_id: "42".to_owned(),
        account_id: AccountId::from("acct_a"),
        folder_id: FolderId::from("folder_inbox"),
        thread_id: None,
        headers: MessageHeaders {
            from: "sender@example.com".to_owned(),
            subject: "Quote request".to_owned(),
            ..MessageHeaders::default()
        },
        body_text: Some("Please send a quote.".to_owned()),
        attachments: vec![Attachment {
            filename: "rfq.pdf".to_owned(),
            content_type: "application/pdf".to_owned(),
            size_bytes: 2048,
        }],
        remote_content_loaded: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_message_is_deterministic() {
        assert_eq!(sample_message(), sample_message());
        assert_eq!(sample_message().id.unwrap().as_str(), "msg_sample");
    }
}
