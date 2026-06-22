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
    /// The model's short "why this draft" explanation, when it supplied one. Empty when the
    /// model omitted it — the review surface degrades to a generic line rather than fabricating
    /// a rationale.
    #[serde(default)]
    pub rationale: String,
}

impl DraftedReply {
    /// A draft with no safety notes.
    #[must_use]
    pub fn new(subject: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            body: body.into(),
            safety_notes: Vec::new(),
            rationale: String::new(),
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
    /// The "why this draft" rationale carried up from the drafter (empty when none was given).
    #[serde(default)]
    pub rationale: String,
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
            rationale: drafted.rationale,
            requires_human_review: true,
        }
    }
}

/// One of the four classes of commitment the compose-review guard watches for in a draft body.
///
/// These are the product's hard promise: before a reply is sent, the user can see exactly what
/// it commits them to. The scanner is **model-free** (determinism-first) — a draft never silently
/// commits a date, a price, a payment term, or a legal position without the guard naming it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentCategory {
    /// A date or deadline ("by Friday", "next week", "March 3", "2026-06-22").
    Date,
    /// A price or monetary amount ("$500", "EUR 1,000", "20% off").
    Price,
    /// A payment term ("net 30", "invoice", "deposit", "refund", "wire transfer").
    Payment,
    /// Binding / legal language ("I agree", "we guarantee", "legally binding", "warrant").
    Legal,
}

impl CommitmentCategory {
    /// Every category, in the order the UI presents them.
    pub const ALL: [Self; 4] = [Self::Date, Self::Price, Self::Payment, Self::Legal];

    /// The stable wire/i18n token for this category.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::Price => "price",
            Self::Payment => "payment",
            Self::Legal => "legal",
        }
    }
}

/// One thing the guard found: the category and the exact span of text that triggered it, cited
/// by char offsets into the body so the review surface can highlight it in place. The matched
/// `text` is carried verbatim so a renderer can show the cited span without re-slicing the body.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct CommitmentFinding {
    /// Which class of commitment this is.
    pub category: CommitmentCategory,
    /// The matched text, verbatim.
    pub text: String,
    /// Char offset (inclusive) of the match start in the body.
    pub start: usize,
    /// Char offset (exclusive) of the match end in the body.
    pub end: usize,
}

/// The guard's verdict over one draft body: every commitment it found, in body order.
///
/// A clear report ([`Self::is_clear`]) is the all-green case; a non-empty one is the "things to
/// check before sending" surface. The report makes no judgement about whether a commitment is
/// *wrong* — only that the draft makes it — which is the honest line for an advisory guard.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct CommitmentGuardReport {
    /// Findings in body order (ascending `start`).
    #[serde(default)]
    pub findings: Vec<CommitmentFinding>,
}

impl CommitmentGuardReport {
    /// Whether the draft made no detectable commitment (the all-clear case).
    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.findings.is_empty()
    }

    /// How many commitments were found.
    #[must_use]
    pub fn count(&self) -> usize {
        self.findings.len()
    }

    /// The distinct categories present, in [`CommitmentCategory::ALL`] order.
    #[must_use]
    pub fn categories(&self) -> Vec<CommitmentCategory> {
        CommitmentCategory::ALL
            .into_iter()
            .filter(|cat| self.findings.iter().any(|f| f.category == *cat))
            .collect()
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

    #[test]
    fn from_drafted_carries_the_rationale_up() {
        let drafted = DraftedReply {
            rationale: "Declined politely; matches your past replies.".to_owned(),
            ..DraftedReply::new("Re: Quote", "No thanks.")
        };
        let draft = ReplyDraft::from_drafted(DraftId::from("d1"), drafted);
        assert_eq!(
            draft.rationale,
            "Declined politely; matches your past replies."
        );
    }

    #[test]
    fn an_empty_guard_report_is_clear_and_has_no_categories() {
        let report = CommitmentGuardReport::default();
        assert!(report.is_clear());
        assert_eq!(report.count(), 0);
        assert!(report.categories().is_empty());
    }

    #[test]
    fn guard_report_categories_are_distinct_and_in_canonical_order() {
        let report = CommitmentGuardReport {
            findings: vec![
                CommitmentFinding {
                    category: CommitmentCategory::Legal,
                    text: "I agree".to_owned(),
                    start: 0,
                    end: 7,
                },
                CommitmentFinding {
                    category: CommitmentCategory::Date,
                    text: "Friday".to_owned(),
                    start: 10,
                    end: 16,
                },
                CommitmentFinding {
                    category: CommitmentCategory::Date,
                    text: "Monday".to_owned(),
                    start: 20,
                    end: 26,
                },
            ],
        };
        assert!(!report.is_clear());
        assert_eq!(report.count(), 3);
        // Distinct, and Date precedes Legal regardless of discovery order.
        assert_eq!(
            report.categories(),
            vec![CommitmentCategory::Date, CommitmentCategory::Legal]
        );
    }

    #[test]
    fn commitment_category_tokens_are_stable() {
        assert_eq!(CommitmentCategory::Date.as_str(), "date");
        assert_eq!(CommitmentCategory::Price.as_str(), "price");
        assert_eq!(CommitmentCategory::Payment.as_str(), "payment");
        assert_eq!(CommitmentCategory::Legal.as_str(), "legal");
    }
}
