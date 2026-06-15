//! The follow-up workflow vocabulary: a versioned cadence ([`WorkflowDefinition`] +
//! immutable [`WorkflowDefinitionVersion`]), its running state machine
//! ([`WorkflowInstance`] — the one mutable-state row and the durable temporal trigger),
//! and the supporting records (conflicts, shadow outcomes, exit events, the drain report).
//!
//! A `WorkflowDefinition` **reuses** the rule version-immutability, lifecycle status set
//! ([`RuleStatus`]), and risk vocabulary ([`RiskLevel`]) — but it is a *cadence*, not a
//! condition→effect rule. Only the optional [`enrollment_condition`] is a JSON-AST
//! [`Condition`]; the cadence steps are absolute day-offsets from an anchor. The fired
//! follow-up is produced by the existing reply-drafter and is review-required by
//! construction — there is no send vocabulary here (see *Sales Pipeline and Follow-up
//! Workflows*).
//!
//! [`enrollment_condition`]: WorkflowDefinitionVersion::enrollment_condition

use serde::{Deserialize, Serialize};

use crate::actor::Actor;
use crate::ids::{
    PipelineItemId, ThreadId, WorkflowConflictId, WorkflowDefId, WorkflowDefVersionId,
    WorkflowInstanceId, WorkflowShadowOutcomeId,
};
use crate::pipeline::ItemType;
use crate::reply::ReplyDraft;
use crate::rules::condition::Condition;
use crate::rules::rule::{RiskLevel, RuleScope, RuleStatus};
use crate::time::Timestamp;

/// The timestamp a cadence's day-offsets count from.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowAnchor {
    /// When the quote was sent.
    QuoteSentAt,
    /// The most recent outbound message on the thread.
    LastOutboundAt,
    /// When the pipeline item was created.
    ItemCreatedAt,
}

impl WorkflowAnchor {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QuoteSentAt => "quote_sent_at",
            Self::LastOutboundAt => "last_outbound_at",
            Self::ItemCreatedAt => "item_created_at",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "quote_sent_at" => Some(Self::QuoteSentAt),
            "last_outbound_at" => Some(Self::LastOutboundAt),
            "item_created_at" => Some(Self::ItemCreatedAt),
            _ => None,
        }
    }
}

/// One cadence step: an **absolute** day-offset from the anchor, a draft intent, and the
/// commitments the draft must never make. `offset_days` is from the anchor, *not* "prior
/// step + N" — so a closed client never gets a backlog of nagging drafts (see the
/// catch-up/coalescing guard).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FollowUpStep {
    /// Position in the cadence (0-based, monotonic).
    pub step_index: i64,
    /// Days after the anchor this step is due.
    pub offset_days: i64,
    /// The drafting intent (`gentle_check_in`, `value_add`, `last_call`, …).
    pub draft_intent: String,
    /// The prompt-template ref the drafter uses, if pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_template_ref: Option<String>,
    /// Commitments the follow-up draft must never make (enforced by the existing
    /// draft-safety path, threaded into the drafter's guidance).
    #[serde(default)]
    pub forbidden_commitments: Vec<String>,
}

/// A condition that exits a sequence.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitCondition {
    /// The counterparty replied on the thread.
    ReplyReceived,
    /// The deal was marked won.
    Won,
    /// The deal was marked lost.
    Lost,
    /// The user cancelled the sequence.
    UserCancel,
    /// The cadence ran out of steps.
    MaxSteps,
}

impl ExitCondition {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReplyReceived => "reply_received",
            Self::Won => "won",
            Self::Lost => "lost",
            Self::UserCancel => "user_cancel",
            Self::MaxSteps => "max_steps",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "reply_received" => Some(Self::ReplyReceived),
            "won" => Some(Self::Won),
            "lost" => Some(Self::Lost),
            "user_cancel" => Some(Self::UserCancel),
            "max_steps" => Some(Self::MaxSteps),
            _ => None,
        }
    }
}

/// The staleness/coalescing guard configuration. With `coalesce = true`, multiple overdue
/// steps within the horizon collapse to one current draft; a step overdue past
/// `abandon_horizon_days` surfaces no draft and moves the instance to `needs_attention`.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct Staleness {
    /// Whether to coalesce multiple overdue fresh steps into the latest one.
    pub coalesce: bool,
    /// Days after which an overdue step is abandoned instead of drafted.
    pub abandon_horizon_days: i64,
}

impl Default for Staleness {
    fn default() -> Self {
        Self {
            coalesce: true,
            abandon_horizon_days: 14,
        }
    }
}

/// The immutable content of a workflow definition version — the cadence itself. Shared by
/// [`NewWorkflowDefinition`] (version 1) and [`NewWorkflowDefVersion`] (a later revision)
/// so both write the same `workflow_definition_versions` columns.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkflowVersionContent {
    /// Human title.
    pub title: String,
    /// Explanation.
    pub description: String,
    /// What the offsets count from.
    pub anchor: WorkflowAnchor,
    /// Optional JSON-AST condition for auto-suggesting enrollment (reuses the rule AST).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrollment_condition: Option<Condition>,
    /// The ordered cadence steps.
    pub steps: Vec<FollowUpStep>,
    /// What exits the sequence.
    pub exit_conditions: Vec<ExitCondition>,
    /// The staleness/coalescing guard.
    pub staleness: Staleness,
    /// The risk band (follow-up drafting is `medium` by default).
    pub risk_level: RiskLevel,
    /// Why this version exists.
    pub change_reason: String,
    /// Who authored it.
    pub created_by: Actor,
}

impl WorkflowVersionContent {
    /// The highest `step_index` in the cadence, or `None` if there are no steps.
    #[must_use]
    pub fn last_step_index(&self) -> Option<i64> {
        self.steps.iter().map(|s| s.step_index).max()
    }

    /// The step at `step_index`, if present.
    #[must_use]
    pub fn step(&self, step_index: i64) -> Option<&FollowUpStep> {
        self.steps.iter().find(|s| s.step_index == step_index)
    }
}

/// An immutable, persisted workflow-definition version (content plus the id and monotonic
/// number the repository stamps).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkflowDefinitionVersion {
    /// The version id (`wfdv_…`).
    pub id: WorkflowDefVersionId,
    /// The owning definition.
    pub workflow_id: WorkflowDefId,
    /// Monotonic version number within the definition.
    pub version_number: i64,
    /// The cadence content.
    pub content: WorkflowVersionContent,
    /// When created.
    pub created_at: Timestamp,
}

/// Current metadata for a versioned follow-up cadence (mirrors `classification_rules`).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkflowDefinition {
    /// The definition id (`wfd_…`).
    pub id: WorkflowDefId,
    /// Stable name ("standard-quote-follow-up").
    pub stable_name: String,
    /// Scope (reuses [`RuleScope`]).
    pub scope: RuleScope,
    /// Which item type the cadence applies to.
    pub applies_to_item_type: ItemType,
    /// The lifecycle status (reuses the rule lifecycle).
    pub status: RuleStatus,
    /// The current version pointer.
    pub current_version_id: WorkflowDefVersionId,
    /// Who created it (`user`, or `ai` for a curator proposal — activation needs review).
    pub created_by: Actor,
    /// When created.
    pub created_at: Timestamp,
    /// When last updated.
    pub updated_at: Timestamp,
}

/// A new definition to persist: identity/scope metadata plus the content of version 1. The
/// repository creates the definition in [`Draft`](RuleStatus::Draft) status and its first
/// immutable version atomically.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct NewWorkflowDefinition {
    /// Stable name.
    pub stable_name: String,
    /// Scope.
    pub scope: RuleScope,
    /// Which item type the cadence applies to.
    pub applies_to_item_type: ItemType,
    /// Who created it.
    pub created_by: Actor,
    /// The content of version 1.
    pub initial_version: WorkflowVersionContent,
}

/// A new immutable version appended to an existing definition. The repository assigns the
/// next monotonic `version_number` and re-points the definition's `current_version_id`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct NewWorkflowDefVersion {
    /// The definition to append to.
    pub workflow_id: WorkflowDefId,
    /// The new cadence content.
    pub content: WorkflowVersionContent,
}

/// The running state of a follow-up sequence. `next_due_at` is non-NULL **iff**
/// `status ∈ {active, snoozed}` — exactly the rows the scheduler selects.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowInstanceStatus {
    /// Armed and ticking toward the next step.
    Active,
    /// A follow-up draft is pending review (the frequency cap: at most one per instance).
    AwaitingReview,
    /// The counterparty replied — the sequence exited (the user may resume).
    Engaged,
    /// The user pushed the next step out.
    Snoozed,
    /// Overdue past the abandon horizon; surfaced a nudge instead of a draft.
    NeedsAttention,
    /// The cadence finished (or the deal closed).
    Completed,
    /// The user stopped the sequence.
    Cancelled,
}

impl WorkflowInstanceStatus {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::AwaitingReview => "awaiting_review",
            Self::Engaged => "engaged",
            Self::Snoozed => "snoozed",
            Self::NeedsAttention => "needs_attention",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "awaiting_review" => Some(Self::AwaitingReview),
            "engaged" => Some(Self::Engaged),
            "snoozed" => Some(Self::Snoozed),
            "needs_attention" => Some(Self::NeedsAttention),
            "completed" => Some(Self::Completed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// Whether the scheduler selects this row (exactly the statuses with a non-NULL
    /// `next_due_at`).
    #[must_use]
    pub fn is_selectable(self) -> bool {
        matches!(self, Self::Active | Self::Snoozed)
    }

    /// Whether this is a terminal status (no further transitions).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }
}

/// The running follow-up state machine — the durable temporal trigger and the one
/// mutable-state row in an otherwise append-only store.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkflowInstance {
    /// The instance id (`wfi_…`).
    pub id: WorkflowInstanceId,
    /// The pipeline item this sequence chases.
    pub pipeline_item_id: PipelineItemId,
    /// The definition driving it.
    pub workflow_id: WorkflowDefId,
    /// The **canonical** version pin, immutable for the instance's life.
    pub pinned_def_version_id: WorkflowDefVersionId,
    /// The thread reply-exit looks up.
    pub thread_id: ThreadId,
    /// The resolved anchor timestamp the offsets count from.
    pub anchor_at: Timestamp,
    /// The FSM status.
    pub status: WorkflowInstanceStatus,
    /// The cursor: the next step to fire.
    pub current_step_index: i64,
    /// The trigger. Non-NULL iff `status ∈ {active, snoozed}`.
    pub next_due_at: Option<Timestamp>,
    /// When created.
    pub created_at: Timestamp,
    /// When last updated.
    pub updated_at: Timestamp,
}

impl WorkflowInstance {
    /// Whether the instance honours the `next_due_at` non-NULL-iff-selectable invariant.
    #[must_use]
    pub fn honours_due_invariant(&self) -> bool {
        self.status.is_selectable() == self.next_due_at.is_some()
    }
}

/// The fields needed to arm a new instance. The repository stamps the id and timestamps.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct NewWorkflowInstance {
    /// The pipeline item to chase.
    pub pipeline_item_id: PipelineItemId,
    /// The definition driving it.
    pub workflow_id: WorkflowDefId,
    /// The canonical version pin.
    pub pinned_def_version_id: WorkflowDefVersionId,
    /// The thread reply-exit looks up.
    pub thread_id: ThreadId,
    /// The resolved anchor timestamp.
    pub anchor_at: Timestamp,
    /// The initial status (`active` on arm).
    pub status: WorkflowInstanceStatus,
    /// The initial cursor (0 on arm).
    pub current_step_index: i64,
    /// The first due time (the first step's absolute due time on arm).
    pub next_due_at: Option<Timestamp>,
}

/// The kind of a recorded workflow conflict. A workflow conflict is a **containment**
/// check ("don't run two active workflows on one item"), structurally unlike the AST/effect
/// overlap that `rule_conflicts` records.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowConflictKind {
    /// Two active workflows target the same pipeline item.
    ConcurrentActiveWorkflow,
}

impl WorkflowConflictKind {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConcurrentActiveWorkflow => "concurrent_active_workflow",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "concurrent_active_workflow" => Some(Self::ConcurrentActiveWorkflow),
            _ => None,
        }
    }
}

/// Whether a recorded conflict is still open or has been human-resolved.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowConflictStatus {
    /// Detected, not yet reviewed.
    Open,
    /// A human resolved it.
    Resolved,
}

impl WorkflowConflictStatus {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "resolved" => Some(Self::Resolved),
            _ => None,
        }
    }
}

/// A recorded containment conflict between two workflows on one pipeline item.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkflowConflict {
    /// The conflict id (`wcf_…`).
    pub id: WorkflowConflictId,
    /// The item both workflows target.
    pub pipeline_item_id: PipelineItemId,
    /// The first workflow (or active instance) id.
    pub workflow_a_id: String,
    /// The second workflow proposed/armed on the same item.
    pub workflow_b_id: String,
    /// The kind of conflict.
    pub conflict_kind: WorkflowConflictKind,
    /// Open or resolved.
    pub status: WorkflowConflictStatus,
    /// When detected.
    pub detected_at: Timestamp,
}

/// A shadow follow-up step that would have fired but never surfaced a draft. Separate from
/// `shadow_outcomes` because a shadow follow-up step is triggered by *time* and has no
/// message at fire time.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkflowShadowOutcome {
    /// The row id (`wsho_…`).
    pub id: WorkflowShadowOutcomeId,
    /// The shadow workflow that would have fired.
    pub workflow_id: WorkflowDefId,
    /// The exact version.
    pub workflow_version_id: WorkflowDefVersionId,
    /// The item it was shadow-running on.
    pub pipeline_item_id: PipelineItemId,
    /// The thread (no `message_id` — there is no triggering message).
    pub thread_id: ThreadId,
    /// Which step.
    pub step_index: i64,
    /// When the step would have surfaced a draft.
    pub would_fire_at: Timestamp,
    /// Whether a reply had already arrived (the step would have been redundant).
    pub reply_before_fire: bool,
    /// Whether the user manually followed up within ±window of `would_fire_at`.
    pub matched_manual_followup_within_days: Option<i64>,
    /// When recorded.
    pub created_at: Timestamp,
}

/// An event that exits a running sequence. The [`ExitDetector`](crate placeholder) maps it
/// to a new instance status and (for won/lost) a pipeline stage.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitEvent {
    /// A reply landed on the tracked thread → `engaged`.
    ReplyReceived,
    /// The deal was marked won → `completed`.
    Won,
    /// The deal was marked lost → `completed`.
    Lost,
    /// The user cancelled → `cancelled`.
    Cancel,
}

impl ExitEvent {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReplyReceived => "reply_received",
            Self::Won => "won",
            Self::Lost => "lost",
            Self::Cancel => "cancel",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "reply_received" => Some(Self::ReplyReceived),
            "won" => Some(Self::Won),
            "lost" => Some(Self::Lost),
            "cancel" => Some(Self::Cancel),
            _ => None,
        }
    }

    /// The instance status this event drives the sequence into.
    #[must_use]
    pub fn resulting_status(self) -> WorkflowInstanceStatus {
        match self {
            Self::ReplyReceived => WorkflowInstanceStatus::Engaged,
            Self::Won | Self::Lost => WorkflowInstanceStatus::Completed,
            Self::Cancel => WorkflowInstanceStatus::Cancelled,
        }
    }

    /// The pipeline stage this event implies, if it closes/advances the deal.
    #[must_use]
    pub fn resulting_stage(self) -> Option<crate::pipeline::PipelineStage> {
        use crate::pipeline::PipelineStage;
        match self {
            Self::ReplyReceived => Some(PipelineStage::Engaged),
            Self::Won => Some(PipelineStage::Won),
            Self::Lost => Some(PipelineStage::Lost),
            Self::Cancel => None,
        }
    }
}

/// How a human resolved a surfaced follow-up draft (`review_followup`). All three advance
/// the cursor past the surfaced step — none sends automatically; `send` means the human
/// confirmed and dispatched the draft themselves.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewResolution {
    /// The human confirmed and sent the draft.
    Send,
    /// The human edited then sent the draft.
    Edit,
    /// The human skipped this step.
    Skip,
}

impl ReviewResolution {
    /// The stable snake_case label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Edit => "edit",
            Self::Skip => "skip",
        }
    }

    /// Parse a stored label, or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "send" => Some(Self::Send),
            "edit" => Some(Self::Edit),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
}

/// One follow-up step the scheduler fired this drain pass: the produced review-required
/// draft plus the indexes the staleness guard coalesced into it. The host turns this into
/// a `followup_draft_ready` notification.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FiredStep {
    /// The instance that fired.
    pub workflow_instance_id: WorkflowInstanceId,
    /// The item it chases.
    pub pipeline_item_id: PipelineItemId,
    /// The thread.
    pub thread_id: ThreadId,
    /// The fired step's index (the latest fresh due step).
    pub step_index: i64,
    /// Earlier due steps collapsed into this one (recorded as `step_skipped_coalesced`).
    #[serde(default)]
    pub coalesced_from: Vec<i64>,
    /// The review-required draft (`requires_human_review = true` by construction).
    pub draft: ReplyDraft,
}

/// One instance that went stale past the abandon horizon this drain pass. The host turns
/// this into a `followup_needs_attention` notification.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct NeedsAttentionItem {
    /// The instance that went stale.
    pub workflow_instance_id: WorkflowInstanceId,
    /// The item it chases.
    pub pipeline_item_id: PipelineItemId,
    /// Why it needs attention.
    pub reason: String,
    /// The step indexes that were skipped (no draft was produced for any of them).
    #[serde(default)]
    pub skipped_step_indexes: Vec<i64>,
}

/// The result of one scheduler drain pass: the steps that fired (each a review-required
/// draft) and the instances that went stale. Coalescing means a long absence yields at most
/// one fired step per instance, never a backlog.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct DrainReport {
    /// The fired steps (each carries its review-required draft).
    pub fired: Vec<FiredStep>,
    /// The instances that went stale past the horizon (no draft).
    pub needs_attention: Vec<NeedsAttentionItem>,
}

impl DrainReport {
    /// Whether this pass did nothing (no fired steps, no stale instances).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fired.is_empty() && self.needs_attention.is_empty()
    }
}

/// The candidate cadence a curator `new_workflow` proposal carries (analogous to
/// [`RuleDraft`](crate::rules::rule::RuleDraft) for a `new_rule`). On acceptance the review
/// step materializes it into a draft/shadow [`WorkflowDefinition`] — never active.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkflowDraft {
    /// The proposed stable name.
    pub stable_name: String,
    /// The proposed scope.
    pub scope: RuleScope,
    /// Which item type it applies to.
    pub applies_to_item_type: ItemType,
    /// What the offsets count from.
    pub anchor: WorkflowAnchor,
    /// The proposed cadence steps.
    pub steps: Vec<FollowUpStep>,
    /// The proposed exit conditions.
    pub exit_conditions: Vec<ExitCondition>,
    /// The proposed staleness guard.
    pub staleness: Staleness,
    /// The proposed risk band.
    pub risk_level: RiskLevel,
}

impl WorkflowDraft {
    /// Lower the draft into the version-1 content the workflow repository persists. The
    /// `change_reason` explains the provenance; the author is recorded by `created_by`.
    #[must_use]
    pub fn into_version_content(
        self,
        title: String,
        description: String,
        change_reason: String,
        created_by: Actor,
    ) -> WorkflowVersionContent {
        WorkflowVersionContent {
            title,
            description,
            anchor: self.anchor,
            enrollment_condition: None,
            steps: self.steps,
            exit_conditions: self.exit_conditions,
            staleness: self.staleness,
            risk_level: self.risk_level,
            change_reason,
            created_by,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_steps() -> Vec<FollowUpStep> {
        vec![
            FollowUpStep {
                step_index: 0,
                offset_days: 3,
                draft_intent: "gentle_check_in".to_owned(),
                prompt_template_ref: None,
                forbidden_commitments: vec!["prices".to_owned()],
            },
            FollowUpStep {
                step_index: 1,
                offset_days: 7,
                draft_intent: "value_add".to_owned(),
                prompt_template_ref: None,
                forbidden_commitments: vec![],
            },
        ]
    }

    #[test]
    fn enum_labels_round_trip() {
        for a in [
            WorkflowAnchor::QuoteSentAt,
            WorkflowAnchor::LastOutboundAt,
            WorkflowAnchor::ItemCreatedAt,
        ] {
            assert_eq!(WorkflowAnchor::from_db_str(a.as_str()), Some(a));
        }
        for e in [
            ExitCondition::ReplyReceived,
            ExitCondition::Won,
            ExitCondition::Lost,
            ExitCondition::UserCancel,
            ExitCondition::MaxSteps,
        ] {
            assert_eq!(ExitCondition::from_db_str(e.as_str()), Some(e));
        }
        for s in [
            WorkflowInstanceStatus::Active,
            WorkflowInstanceStatus::AwaitingReview,
            WorkflowInstanceStatus::Engaged,
            WorkflowInstanceStatus::Snoozed,
            WorkflowInstanceStatus::NeedsAttention,
            WorkflowInstanceStatus::Completed,
            WorkflowInstanceStatus::Cancelled,
        ] {
            assert_eq!(WorkflowInstanceStatus::from_db_str(s.as_str()), Some(s));
        }
        assert_eq!(WorkflowInstanceStatus::from_db_str("nope"), None);
        let k = WorkflowConflictKind::ConcurrentActiveWorkflow;
        assert_eq!(WorkflowConflictKind::from_db_str(k.as_str()), Some(k));
        for cs in [
            WorkflowConflictStatus::Open,
            WorkflowConflictStatus::Resolved,
        ] {
            assert_eq!(WorkflowConflictStatus::from_db_str(cs.as_str()), Some(cs));
        }
    }

    #[test]
    fn only_active_and_snoozed_are_selectable_and_carry_a_due_time() {
        assert!(WorkflowInstanceStatus::Active.is_selectable());
        assert!(WorkflowInstanceStatus::Snoozed.is_selectable());
        for s in [
            WorkflowInstanceStatus::AwaitingReview,
            WorkflowInstanceStatus::Engaged,
            WorkflowInstanceStatus::NeedsAttention,
            WorkflowInstanceStatus::Completed,
            WorkflowInstanceStatus::Cancelled,
        ] {
            assert!(!s.is_selectable(), "{} must not be selectable", s.as_str());
        }
        assert!(WorkflowInstanceStatus::Completed.is_terminal());
        assert!(WorkflowInstanceStatus::Cancelled.is_terminal());
        assert!(!WorkflowInstanceStatus::Active.is_terminal());
    }

    #[test]
    fn instance_honours_the_due_invariant() {
        let mut inst = WorkflowInstance {
            id: WorkflowInstanceId::from("wfi_1"),
            pipeline_item_id: PipelineItemId::from("pli_1"),
            workflow_id: WorkflowDefId::from("wfd_1"),
            pinned_def_version_id: WorkflowDefVersionId::from("wfdv_1"),
            thread_id: ThreadId::from("thread_1"),
            anchor_at: Timestamp::now(),
            status: WorkflowInstanceStatus::Active,
            current_step_index: 0,
            next_due_at: Some(Timestamp::now()),
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        };
        assert!(inst.honours_due_invariant(), "active + due is valid");
        inst.next_due_at = None;
        assert!(
            !inst.honours_due_invariant(),
            "active without due is invalid"
        );
        inst.status = WorkflowInstanceStatus::AwaitingReview;
        assert!(
            inst.honours_due_invariant(),
            "awaiting_review without due is valid"
        );
    }

    #[test]
    fn exit_event_maps_to_status_and_stage() {
        assert_eq!(
            ExitEvent::ReplyReceived.resulting_status(),
            WorkflowInstanceStatus::Engaged
        );
        assert_eq!(
            ExitEvent::Won.resulting_status(),
            WorkflowInstanceStatus::Completed
        );
        assert_eq!(
            ExitEvent::Cancel.resulting_status(),
            WorkflowInstanceStatus::Cancelled
        );
        assert_eq!(
            ExitEvent::Won.resulting_stage(),
            Some(crate::pipeline::PipelineStage::Won)
        );
        assert_eq!(ExitEvent::Cancel.resulting_stage(), None);
        assert_eq!(
            ExitEvent::from_db_str(ExitEvent::Lost.as_str()),
            Some(ExitEvent::Lost)
        );
    }

    #[test]
    fn review_resolution_round_trips() {
        for r in [
            ReviewResolution::Send,
            ReviewResolution::Edit,
            ReviewResolution::Skip,
        ] {
            assert_eq!(ReviewResolution::from_db_str(r.as_str()), Some(r));
        }
        assert_eq!(ReviewResolution::from_db_str("nope"), None);
    }

    #[test]
    fn version_content_finds_steps_and_last_index() {
        let content = WorkflowVersionContent {
            title: "Standard".to_owned(),
            description: "3/7".to_owned(),
            anchor: WorkflowAnchor::QuoteSentAt,
            enrollment_condition: None,
            steps: sample_steps(),
            exit_conditions: vec![ExitCondition::ReplyReceived],
            staleness: Staleness::default(),
            risk_level: RiskLevel::Medium,
            change_reason: "seed".to_owned(),
            created_by: Actor::User,
        };
        assert_eq!(content.last_step_index(), Some(1));
        assert_eq!(content.step(1).map(|s| s.offset_days), Some(7));
        assert!(content.step(9).is_none());
    }

    #[test]
    fn staleness_default_coalesces_with_a_fortnight_horizon() {
        let s = Staleness::default();
        assert!(s.coalesce);
        assert_eq!(s.abandon_horizon_days, 14);
    }

    #[test]
    fn workflow_draft_lowers_into_version_content() {
        let draft = WorkflowDraft {
            stable_name: "standard-quote-follow-up".to_owned(),
            scope: RuleScope::Global,
            applies_to_item_type: ItemType::Quote,
            anchor: WorkflowAnchor::QuoteSentAt,
            steps: sample_steps(),
            exit_conditions: vec![ExitCondition::ReplyReceived, ExitCondition::Won],
            staleness: Staleness::default(),
            risk_level: RiskLevel::Medium,
        };
        let content = draft.into_version_content(
            "Standard quote follow-up".to_owned(),
            "3 / 7".to_owned(),
            "curated from followup evidence".to_owned(),
            Actor::Ai,
        );
        assert_eq!(content.steps.len(), 2);
        assert_eq!(content.created_by, Actor::Ai);
        assert!(content.enrollment_condition.is_none());
    }

    #[test]
    fn drain_report_empty_detection() {
        assert!(DrainReport::default().is_empty());
    }
}
