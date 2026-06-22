//! The append-only audit timeline: cross-cutting provenance that is *not* task feedback.
//!
//! A policy block, a provider rejection, a rule-lifecycle transition — facts with no
//! per-task feedback home — land here, each exactly once. Corrections never do (they own
//! their feedback table). The unified "everything that happened to this message" view is a
//! read over `audit_log` ∪ the feedback tables; this module is the writer's vocabulary.

use serde::{Deserialize, Serialize};

use crate::actor::Actor;
use crate::ids::{AuditId, MessageId, ProposalId, RuleId, RuleVersionId, ThreadId};
use crate::rules::rule::RuleKind;
use crate::time::Timestamp;

/// Well-known `event_type` values. The column is an open vocabulary (TEXT), but these
/// constants keep the common events spelled identically at every call site.
pub mod event_type {
    /// A safe action was applied to a message.
    pub const ACTION_APPLIED: &str = "action_applied";
    /// An auto-applied action was undone by the user — the strongest negative signal against the
    /// rule that authored it. Stamped with that `rule_id`, it is the per-rule decay signal the
    /// learning engine reads to surface a *retire* proposal. Paired with the per-rule
    /// [`ACTION_APPLIED`] fires count (now also `rule_id`-stamped), it yields the real undo *rate*
    /// the decay pass governs on; the undo *count* is the fallback only for a rule with no recorded
    /// fires yet.
    pub const ACTION_UNDONE: &str = "action_undone";
    /// A candidate action was blocked by the policy guard.
    pub const ACTION_BLOCKED_BY_POLICY: &str = "action_blocked_by_policy";
    /// A provider response failed validation and was discarded.
    pub const PROVIDER_RESPONSE_REJECTED: &str = "provider_response_rejected";
    /// The learning engine emitted a rule proposal.
    pub const RULE_PROPOSED: &str = "rule_proposed";
    /// A rule changed lifecycle status (draft → shadow → active → …).
    pub const RULE_STATUS_CHANGED: &str = "rule_status_changed";
    /// A human activated a rule — the separate, explicit decision that turns an accepted
    /// proposal's rule live (distinct from the shadow/pending materialization). This is what a
    /// trust receipt counts as "rules you turned on", and what an audit reads to prove no rule
    /// went active without a deliberate human action.
    pub const RULE_ACTIVATED: &str = "rule_activated";
    /// A proposal was reviewed (accepted/rejected) by a human.
    pub const PROPOSAL_REVIEWED: &str = "proposal_reviewed";
    /// The curator recorded a conflict between two live rules.
    pub const RULE_CONFLICT_DETECTED: &str = "rule_conflict_detected";
    /// The user *sent* a message — outbound evidence. One row per recipient (its domain in the
    /// payload). Counting these per domain is how the learning engine proposes a VIP/priority rule:
    /// people you repeatedly email are people whose mail matters (learn-from-Sent, Phase 7).
    pub const MAIL_SENT: &str = "mail_sent";
    /// An on-device Tier-2 model training run completed (Phase 8). The payload carries the
    /// held-out metrics and whether the artifact cleared the precision gate and was activated —
    /// the auditable record that "the gate flips active only above threshold".
    pub const TIER2_TRAINED: &str = "tier2_trained";
    /// The user erased data about a message or a sender, or reset all learning (Phase 9
    /// delete-my-data). The payload carries the scope and the per-table tally of what was
    /// removed. A single tombstone left behind after the per-message audit rows are deleted, so
    /// "this data was forgotten" stays accountable without resurrecting what was erased.
    pub const DATA_FORGOTTEN: &str = "data_forgotten";
    /// The user exported everything stored about them (Phase 9 portability). The payload carries
    /// only counts — never the exported content — so the audit log itself is not a copy of the
    /// data the user asked to take with them.
    pub const DATA_EXPORTED: &str = "data_exported";
    /// A durable remind-me / snooze timer came due and the host emitted its notify-only nudge
    /// (Phase 9). One row per fired reminder — the accountable record that the nudge happened.
    pub const REMINDER_FIRED: &str = "reminder_fired";
}

/// One append-only audit entry.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AuditEntry {
    /// The entry id (`audit_…`).
    pub id: AuditId,
    /// What happened (see [`event_type`]).
    pub event_type: String,
    /// The message it concerns, if any.
    pub message_id: Option<MessageId>,
    /// The thread it concerns, if any.
    pub thread_id: Option<ThreadId>,
    /// The rule kind, when a rule is referenced (disambiguates `rule_id`).
    pub rule_kind: Option<RuleKind>,
    /// The rule it concerns, if any.
    pub rule_id: Option<RuleId>,
    /// The exact rule version, if any.
    pub rule_version_id: Option<RuleVersionId>,
    /// The proposal it concerns, if any.
    pub proposal_id: Option<ProposalId>,
    /// Who caused it.
    pub actor: Actor,
    /// Event-specific structured data (opaque JSON).
    pub payload: serde_json::Value,
    /// When it happened.
    pub created_at: Timestamp,
}

impl AuditEntry {
    /// A fresh entry stamped now, with a fresh id and an empty payload.
    #[must_use]
    pub fn new(event_type: impl Into<String>, actor: Actor) -> Self {
        Self {
            id: AuditId::fresh(),
            event_type: event_type.into(),
            message_id: None,
            thread_id: None,
            rule_kind: None,
            rule_id: None,
            rule_version_id: None,
            proposal_id: None,
            actor,
            payload: serde_json::Value::Null,
            created_at: Timestamp::now(),
        }
    }

    /// Attach the message this entry concerns.
    #[must_use]
    pub fn with_message(mut self, message_id: MessageId) -> Self {
        self.message_id = Some(message_id);
        self
    }

    /// Attach the rule (and its kind) this entry concerns.
    #[must_use]
    pub fn with_rule(mut self, kind: RuleKind, rule_id: RuleId) -> Self {
        self.rule_kind = Some(kind);
        self.rule_id = Some(rule_id);
        self
    }

    /// Attach the proposal this entry concerns.
    #[must_use]
    pub fn with_proposal(mut self, proposal_id: ProposalId) -> Self {
        self.proposal_id = Some(proposal_id);
        self
    }

    /// Attach the structured payload.
    #[must_use]
    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = payload;
        self
    }
}

/// A filter over the audit timeline.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct AuditQuery {
    /// Restrict to one event type.
    pub event_type: Option<String>,
    /// Restrict to one message.
    pub message_id: Option<MessageId>,
    /// Restrict to one rule.
    pub rule_id: Option<RuleId>,
    /// Cap the number of entries (newest first).
    pub limit: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::rule::RuleKind;

    #[test]
    fn builder_sets_only_the_attached_fields() {
        let entry = AuditEntry::new(event_type::RULE_PROPOSED, Actor::Ai)
            .with_message(MessageId::from("msg_7"))
            .with_rule(RuleKind::Action, RuleId::from("rule_9"))
            .with_payload(serde_json::json!({ "candidate": "file_receipts" }));
        assert_eq!(entry.event_type, "rule_proposed");
        assert_eq!(entry.actor, Actor::Ai);
        assert_eq!(entry.message_id, Some(MessageId::from("msg_7")));
        assert_eq!(entry.rule_kind, Some(RuleKind::Action));
        assert_eq!(entry.thread_id, None);
        assert_eq!(entry.payload["candidate"], "file_receipts");
        assert!(entry.id.as_str().starts_with("audit_"));
    }
}
