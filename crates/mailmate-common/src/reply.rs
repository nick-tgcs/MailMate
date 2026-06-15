//! The reply-drafting vocabulary: the bounded, redacted request the host hands a
//! [`ReplyDrafter`](crate) and the advisory draft it returns.
//!
//! A draft is **always** advisory: it is produced for human review and can never be sent
//! by MailMate (`never_auto_send_drafts`). The drafter returns only the text it generated
//! ([`DraftedReply`]); the host wraps it into a [`ReplyDraft`] with a fresh id and the
//! review flag pinned on, so "requires review" is a property of the type, not of any one
//! model's cooperation.
//!
//! [`ReplyDrafter`]: ../../mailmate_ports/reply_drafter/trait.ReplyDrafter.html

use serde::{Deserialize, Serialize};

use crate::ids::{DraftId, MessageId, ThreadId};

/// The context the host assembles for a `draft_reply` request.
///
/// The excerpt is already bounded and retention-clamped by the caller — the drafter never
/// reaches past these fields to the raw mailbox. `forbidden_commitments` names the classes
/// of statement the reply must not make (dates, prices, payment changes, legal positions);
/// they are threaded into the model guidance and echoed in the draft's safety notes.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplyDraftRequest {
    /// The thread being replied to, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    /// The specific message the reply answers, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<MessageId>,
    /// The subject being replied to.
    pub subject: String,
    /// The counterparty address the reply is addressed to.
    pub counterparty: String,
    /// A bounded, retention-clamped excerpt of the message/thread to reply to.
    pub excerpt: String,
    /// Optional user guidance steering tone/content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_instruction: Option<String>,
    /// Classes of commitment the reply must not make.
    #[serde(default)]
    pub forbidden_commitments: Vec<String>,
}

impl ReplyDraftRequest {
    /// A minimal request: reply to `subject` from `counterparty` over `excerpt`.
    #[must_use]
    pub fn new(
        subject: impl Into<String>,
        counterparty: impl Into<String>,
        excerpt: impl Into<String>,
    ) -> Self {
        Self {
            subject: subject.into(),
            counterparty: counterparty.into(),
            excerpt: excerpt.into(),
            ..Self::default()
        }
    }
}

/// The raw text a [`ReplyDrafter`](crate) produced: subject, body, and the safety notes the
/// model attached. It carries no id and no review flag — those are the host's to assign, so
/// a drafter can never hand back something that looks "ready to send".
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DraftedReply {
    /// The reply subject (typically `Re: …`).
    pub subject: String,
    /// The reply body (plain text).
    pub body: String,
    /// Human-facing notes asserting which forbidden commitments were avoided.
    #[serde(default)]
    pub safety_notes: Vec<String>,
}

impl DraftedReply {
    /// A draft with no safety notes.
    #[must_use]
    pub fn new(subject: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            body: body.into(),
            safety_notes: Vec::new(),
        }
    }
}

/// The host-owned, review-required reply the `draft_reply` response carries.
///
/// `requires_human_review` is constructed `true` by [`ReplyDraft::from_drafted`] and there is
/// no constructor that sets it false — the only way to lower it is a deliberate field write,
/// which is the visible change the invariant demands.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplyDraft {
    /// The host-assigned draft id (correlates the response to a later review/edit).
    pub draft_id: DraftId,
    /// The reply subject.
    pub subject: String,
    /// The reply body (plain text).
    pub body: String,
    /// The safety notes carried up from the drafter.
    #[serde(default)]
    pub safety_notes: Vec<String>,
    /// Always `true`: a MailMate draft is never auto-sent.
    pub requires_human_review: bool,
}

impl ReplyDraft {
    /// Wrap a [`DraftedReply`] under `draft_id`, pinning `requires_human_review = true`.
    #[must_use]
    pub fn from_drafted(draft_id: DraftId, drafted: DraftedReply) -> Self {
        Self {
            draft_id,
            subject: drafted.subject,
            body: drafted.body,
            safety_notes: drafted.safety_notes,
            requires_human_review: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_new_sets_the_core_fields_and_defaults_the_rest() {
        let req = ReplyDraftRequest::new("Re: Quote", "buyer@acme.test", "Please advise.");
        assert_eq!(req.subject, "Re: Quote");
        assert_eq!(req.counterparty, "buyer@acme.test");
        assert!(req.forbidden_commitments.is_empty());
        assert!(req.user_instruction.is_none());
        assert!(req.thread_id.is_none());
    }

    #[test]
    fn from_drafted_always_requires_human_review() {
        let drafted = DraftedReply {
            safety_notes: vec!["no prices added".to_owned()],
            ..DraftedReply::new("Re: Quote", "Hi,\n\nThanks.")
        };
        let draft = ReplyDraft::from_drafted(DraftId::from("draft_1"), drafted);
        assert!(
            draft.requires_human_review,
            "a draft is never auto-sendable"
        );
        assert_eq!(draft.subject, "Re: Quote");
        assert_eq!(draft.safety_notes, vec!["no prices added".to_owned()]);
    }

    #[test]
    fn reply_draft_round_trips_through_json() {
        let draft = ReplyDraft::from_drafted(
            DraftId::from("draft_9"),
            DraftedReply::new("Re: Hi", "Body"),
        );
        let back: ReplyDraft =
            serde_json::from_str(&serde_json::to_string(&draft).unwrap()).unwrap();
        assert_eq!(back, draft);
    }
}
