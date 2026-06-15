//! The sales-pipeline item vocabulary — a deliberately minimal tracker, **not a CRM**.
//!
//! A [`PipelineItem`] is a tracked quote/proposal: a stage, the outbound thread it
//! anchors to, the counterparty, and a display-only amount hint. There are no contacts,
//! line-items, or revenue forecasting here (see *Sales Pipeline and Follow-up Workflows*).
//! An item is always created by a `user` (never `ai`); a [`crate::workflow::WorkflowInstance`]
//! is armed on it to drive review-required follow-up drafts.

use serde::{Deserialize, Serialize};

use crate::actor::Actor;
use crate::ids::{MessageId, PipelineItemId, ThreadId};
use crate::time::Timestamp;

/// What kind of tracked deal this is. A shared enum reused by the workflow definition's
/// `applies_to_item_type`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemType {
    /// A sent quote.
    Quote,
    /// A sent proposal.
    Proposal,
}

impl ItemType {
    /// The stable snake_case label stored in a `TEXT` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quote => "quote",
            Self::Proposal => "proposal",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "quote" => Some(Self::Quote),
            "proposal" => Some(Self::Proposal),
            _ => None,
        }
    }
}

/// Where a tracked deal sits. `open` while it is being chased; `engaged` once the
/// counterparty replies; terminal on `won`/`lost`/`abandoned`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStage {
    /// Quote/proposal sent, no reply yet — follow-ups are armed.
    Open,
    /// The counterparty replied (the sequence exits to let the human take over).
    Engaged,
    /// Closed-won.
    Won,
    /// Closed-lost.
    Lost,
    /// Dropped without a decision (e.g. went stale past the abandon horizon).
    Abandoned,
}

impl PipelineStage {
    /// The stable snake_case label stored in a `TEXT` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Engaged => "engaged",
            Self::Won => "won",
            Self::Lost => "lost",
            Self::Abandoned => "abandoned",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "engaged" => Some(Self::Engaged),
            "won" => Some(Self::Won),
            "lost" => Some(Self::Lost),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }

    /// Whether the deal has reached a terminal stage (no further follow-ups should arm).
    #[must_use]
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Won | Self::Lost | Self::Abandoned)
    }
}

/// A tracked quote/proposal. The fields are the whole feature: stage, thread anchor,
/// counterparty, and an opaque amount hint — nothing forecast-shaped.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PipelineItem {
    /// The item id (`pli_…`).
    pub id: PipelineItemId,
    /// The account this deal belongs to.
    pub account_id: String,
    /// The outbound quote/proposal thread (reply-exit keys off this).
    pub thread_id: ThreadId,
    /// The sent quote message, when known (the follow-up draft's `in_reply_to` anchor).
    pub anchor_message_id: Option<MessageId>,
    /// Who we follow up with (readable).
    pub counterparty_email: String,
    /// The counterparty's domain (readable; deterministic clustering).
    pub counterparty_domain: String,
    /// A short human title ("Acme — 40-lane SCO quote").
    pub title: String,
    /// Quote or proposal.
    pub item_type: ItemType,
    /// The current stage.
    pub stage: PipelineStage,
    /// A display-only amount hint — **not** a forecast field.
    pub amount_hint: Option<String>,
    /// The last activity timestamp on this deal.
    pub last_activity_at: Timestamp,
    /// Who created it — always `user`, never `ai`.
    pub created_by: Actor,
    /// When created.
    pub created_at: Timestamp,
    /// When last updated.
    pub updated_at: Timestamp,
}

/// The fields needed to enroll a new pipeline item. The repository stamps the id and the
/// `created_at`/`updated_at`/`last_activity_at` timestamps; `created_by` is pinned to
/// `user` at the wire boundary (the host never lets the AI enroll an item).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct NewPipelineItem {
    /// The account this deal belongs to.
    pub account_id: String,
    /// The outbound quote/proposal thread.
    pub thread_id: ThreadId,
    /// The sent quote message, when known.
    pub anchor_message_id: Option<MessageId>,
    /// Who we follow up with.
    pub counterparty_email: String,
    /// The counterparty's domain.
    pub counterparty_domain: String,
    /// A short human title.
    pub title: String,
    /// Quote or proposal.
    pub item_type: ItemType,
    /// A display-only amount hint.
    pub amount_hint: Option<String>,
}

/// A query over `pipeline_items`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct PipelineItemQuery {
    /// Restrict to one account.
    pub account_id: Option<String>,
    /// Restrict to one stage.
    pub stage: Option<PipelineStage>,
    /// Cap the number of rows (newest first).
    pub limit: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_type_and_stage_labels_round_trip() {
        for it in [ItemType::Quote, ItemType::Proposal] {
            assert_eq!(ItemType::from_db_str(it.as_str()), Some(it));
        }
        assert_eq!(ItemType::from_db_str("nope"), None);
        for stage in [
            PipelineStage::Open,
            PipelineStage::Engaged,
            PipelineStage::Won,
            PipelineStage::Lost,
            PipelineStage::Abandoned,
        ] {
            assert_eq!(PipelineStage::from_db_str(stage.as_str()), Some(stage));
        }
        assert_eq!(PipelineStage::from_db_str("nope"), None);
    }

    #[test]
    fn only_won_lost_abandoned_are_closed() {
        assert!(PipelineStage::Won.is_closed());
        assert!(PipelineStage::Lost.is_closed());
        assert!(PipelineStage::Abandoned.is_closed());
        assert!(!PipelineStage::Open.is_closed());
        assert!(!PipelineStage::Engaged.is_closed());
    }

    #[test]
    fn pipeline_item_round_trips_through_serde() {
        let item = PipelineItem {
            id: PipelineItemId::from("pli_1"),
            account_id: "acct_default".to_owned(),
            thread_id: ThreadId::from("thread_1"),
            anchor_message_id: Some(MessageId::from("msg_1")),
            counterparty_email: "buyer@acme.test".to_owned(),
            counterparty_domain: "acme.test".to_owned(),
            title: "Acme — SCO quote".to_owned(),
            item_type: ItemType::Quote,
            stage: PipelineStage::Open,
            amount_hint: Some("$40k".to_owned()),
            last_activity_at: Timestamp::now(),
            created_by: Actor::User,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: PipelineItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, item);
    }
}
