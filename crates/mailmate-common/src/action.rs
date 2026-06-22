//! The action vocabularies the policy guard reconciles.
//!
//! Two layers, deliberately distinct:
//!
//! - [`ProposedAction`] is the **untrusted candidate** vocabulary — what a rule effect, an
//!   AI proposal, or a protocol request might ask for. It can name prohibited acts
//!   (delete, send, open-link, remote-content download, payment-detail trust) precisely so
//!   the guard can refuse them. This is defense in depth: even though today's structured
//!   rule/AI vocabularies omit those acts, the guard enumerates and blocks them, so any
//!   future path that produces one is caught.
//! - [`PlannedAction`] is the **safe output** vocabulary — the five kinds MailMate may
//!   actually apply (no `Delete`, no `SendDraft`). A prohibited candidate can never become
//!   a `PlannedAction`; it ends up in [`GuardedActionPlan::blocked_actions`].
//!
//! > **Naming note.** `architecture.md` calls the input element `PlannedAction` while also
//! > defining `PlannedAction` as the safe-only set — the two cannot both hold, since the
//! > guard must block acts the safe set cannot express. We split them: untrusted input is
//! > `ProposedAction`, safe output is `PlannedAction`.

use serde::{Deserialize, Serialize};

use crate::ids::{DecisionId, DraftId, FolderId, MessageId, RuleId};
use crate::mail::DraftSpec;
use crate::policy::PolicyCheckResult;

/// An untrusted candidate action submitted to the policy guard.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProposedAction {
    /// Apply a tag.
    Tag {
        /// Target message.
        message_id: MessageId,
        /// Tag key.
        tag: String,
    },
    /// Move a message to a folder.
    Move {
        /// Target message.
        message_id: MessageId,
        /// Destination folder.
        to_folder: FolderId,
    },
    /// Mark (or unmark) a message as junk.
    MarkJunk {
        /// Target message.
        message_id: MessageId,
        /// Junk state to set.
        junk: bool,
    },
    /// Persist a draft (always review-required; never sent).
    CreateDraft {
        /// The draft to persist.
        draft: DraftSpec,
    },
    /// Flag a target for human review.
    RequireReview {
        /// A human-readable description of what needs review.
        target: String,
    },
    /// Delete a message — forbidden by `never_auto_delete_mail`.
    Delete {
        /// Target message.
        message_id: MessageId,
    },
    /// Send a draft — forbidden by `never_auto_send_drafts`.
    SendDraft {
        /// The draft that would be sent.
        draft_id: DraftId,
    },
    /// Open a link from a message — forbidden by `never_open_links`.
    OpenLink {
        /// Target message.
        message_id: MessageId,
        /// The URL that would be opened.
        url: String,
    },
    /// Download remote content for classification — forbidden by
    /// `never_download_remote_content_for_classification`.
    DownloadRemoteContent {
        /// Target message.
        message_id: MessageId,
    },
    /// Automatically trust a payment-detail change — forbidden by
    /// `never_auto_trust_payment_detail_changes`.
    TrustPaymentDetailChange {
        /// Target message.
        message_id: MessageId,
    },
}

impl ProposedAction {
    /// Project a candidate onto the safe output vocabulary, or `None` if the candidate is a
    /// prohibited act that can never be applied.
    #[must_use]
    pub fn to_planned(&self) -> Option<PlannedAction> {
        match self {
            Self::Tag { message_id, tag } => Some(PlannedAction::Tag {
                message_id: message_id.clone(),
                tag: tag.clone(),
            }),
            Self::Move {
                message_id,
                to_folder,
            } => Some(PlannedAction::Move {
                message_id: message_id.clone(),
                to_folder: to_folder.clone(),
            }),
            Self::MarkJunk { message_id, junk } => Some(PlannedAction::MarkJunk {
                message_id: message_id.clone(),
                junk: *junk,
            }),
            Self::CreateDraft { draft } => Some(PlannedAction::CreateDraft {
                draft: draft.clone(),
            }),
            Self::RequireReview { target } => Some(PlannedAction::RequireReview {
                target: target.clone(),
            }),
            Self::Delete { .. }
            | Self::SendDraft { .. }
            | Self::OpenLink { .. }
            | Self::DownloadRemoteContent { .. }
            | Self::TrustPaymentDetailChange { .. } => None,
        }
    }
}

/// A safe action MailMate may apply (allowed) or surface for confirmation (review).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlannedAction {
    /// Apply a tag.
    Tag {
        /// Target message.
        message_id: MessageId,
        /// Tag key.
        tag: String,
    },
    /// Move a message to a folder.
    Move {
        /// Target message.
        message_id: MessageId,
        /// Destination folder.
        to_folder: FolderId,
    },
    /// Mark (or unmark) a message as junk.
    MarkJunk {
        /// Target message.
        message_id: MessageId,
        /// Junk state to set.
        junk: bool,
    },
    /// Persist a draft (always review-required; never sent).
    CreateDraft {
        /// The draft to persist.
        draft: DraftSpec,
    },
    /// Flag a target for human review.
    RequireReview {
        /// A human-readable description of what needs review.
        target: String,
    },
}

/// A candidate plan submitted to the policy guard.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActionPlan {
    /// The decision this plan belongs to.
    pub decision_id: DecisionId,
    /// The message the plan concerns, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    /// The candidate actions to evaluate.
    pub actions: Vec<ProposedAction>,
    /// The authoring rule for each action in `actions` (a length-matched sidecar: `authored_by[i]`
    /// is the rule that produced `actions[i]`, or `None` for an action the planner synthesized —
    /// e.g. a needs-review fallback — that no single rule authored). This is the per-firing
    /// provenance the guard preserves and the apply path stamps onto `action_applied`, so the
    /// Rules manager can count a rule's real fires and undo-rate. Defaulted empty so a plan built
    /// without provenance simply has none (the apply path degrades to `None`, never panics).
    #[serde(default)]
    pub authored_by: Vec<Option<RuleId>>,
}

/// A candidate action the guard refused, with the policy that refused it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlockedAction {
    /// The rejected candidate.
    pub action: ProposedAction,
    /// The hard policy that forbids it.
    pub policy_id: String,
    /// Why it was blocked.
    pub reason: String,
}

/// The result of running a [`PolicyGuard`](crate) over an [`ActionPlan`]: candidates
/// partitioned by outcome, plus the per-policy audit trail.
///
/// [`PolicyGuard`]: crate
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GuardedActionPlan {
    /// The decision this guarded plan belongs to.
    pub decision_id: DecisionId,
    /// Actions MailMate may apply automatically.
    pub allowed_actions: Vec<PlannedAction>,
    /// The authoring rule for each allowed action (a length-matched sidecar:
    /// `allowed_authored_by[i]` authored `allowed_actions[i]`). The guard fills it in lockstep with
    /// `allowed_actions`, carrying the planner's per-action provenance through the partition so the
    /// apply path can stamp each auto-applied action with the rule that fired it. Defaulted empty;
    /// the apply path reads it position-wise and degrades a missing entry to `None`.
    #[serde(default)]
    pub allowed_authored_by: Vec<Option<RuleId>>,
    /// Actions MailMate may apply only after human review.
    pub review_required_actions: Vec<PlannedAction>,
    /// Candidates a hard policy refused.
    pub blocked_actions: Vec<BlockedAction>,
    /// The per-policy checks that produced this partition.
    pub policy_checks: Vec<PolicyCheckResult>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_candidates_project_onto_planned_actions() {
        let tag = ProposedAction::Tag {
            message_id: MessageId::from("msg_1"),
            tag: "receipt".to_owned(),
        };
        assert!(matches!(tag.to_planned(), Some(PlannedAction::Tag { .. })));
    }

    #[test]
    fn prohibited_candidates_have_no_safe_projection() {
        for action in [
            ProposedAction::Delete {
                message_id: MessageId::from("msg_1"),
            },
            ProposedAction::SendDraft {
                draft_id: DraftId::from("draft_1"),
            },
            ProposedAction::OpenLink {
                message_id: MessageId::from("msg_1"),
                url: "http://x".to_owned(),
            },
            ProposedAction::DownloadRemoteContent {
                message_id: MessageId::from("msg_1"),
            },
            ProposedAction::TrustPaymentDetailChange {
                message_id: MessageId::from("msg_1"),
            },
        ] {
            assert!(
                action.to_planned().is_none(),
                "{action:?} must not be applicable"
            );
        }
    }

    #[test]
    fn proposed_action_is_tagged_in_json() {
        let action = ProposedAction::Move {
            message_id: MessageId::from("msg_1"),
            to_folder: FolderId::from("folder_archive"),
        };
        let value = serde_json::to_value(&action).unwrap();
        assert_eq!(value["kind"], "move");
        let back: ProposedAction = serde_json::from_value(value).unwrap();
        assert_eq!(back, action);
    }
}
