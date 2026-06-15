//! Proposal generation: turn a clustered group of feedback into a reviewable
//! [`AgentProposal`] carrying a deterministic [`RuleDraft`] and the evidence rows that
//! justify it.
//!
//! Crystallization step 1 (*express the candidate deterministically*): the condition is a
//! JSON-AST predicate over deterministic features only (`sender_domain`), and the effect is
//! a plain move/label — no clause calls a model. The proposal recommends `shadow_mode`, not
//! activation: the human-review and back-test gates still stand between it and a live rule.

use mailmate_common::evidence::{EvidenceKind, EvidenceSourceKind, RuleEvidence};
use mailmate_common::feedback::FollowUpFeedbackRow;
use mailmate_common::ids::{EvidenceId, ProposalId};
use mailmate_common::proposal::{AgentProposal, EvidenceRef, ProposalKind, ProposalStatus};
use mailmate_common::rules::condition::{Condition, FieldValue, Operator, Predicate};
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::rule::{RiskLevel, RuleDraft, RuleKind, RuleScope, RuleStatus};
use mailmate_common::time::Timestamp;
use mailmate_common::workflow::WorkflowDraft;

use crate::evidence::{ClassificationCluster, FilingCluster};

/// A `sender_domain == <domain>` predicate — the deterministic key both proposal kinds use.
fn sender_domain_eq(domain: &str) -> Condition {
    Condition::Predicate(Predicate {
        field: "sender_domain".to_owned(),
        op: Operator::Eq,
        value: FieldValue::Text(domain.to_owned()),
    })
}

/// Build the evidence rows linking a proposal to the feedback rows that justify it.
fn evidence_rows(
    proposal_id: &ProposalId,
    rule_kind: RuleKind,
    source_kind: EvidenceSourceKind,
    rows: impl Iterator<
        Item = (
            mailmate_common::ids::FeedbackId,
            mailmate_common::ids::MessageId,
            String,
        ),
    >,
) -> (Vec<RuleEvidence>, Vec<EvidenceRef>) {
    let mut evidence = Vec::new();
    let mut refs = Vec::new();
    for (source_id, message_id, summary) in rows {
        refs.push(EvidenceRef {
            kind: source_kind,
            id: source_id.clone(),
        });
        evidence.push(RuleEvidence {
            id: EvidenceId::fresh(),
            rule_kind: Some(rule_kind),
            rule_id: None,
            proposal_id: Some(proposal_id.clone()),
            source_kind,
            source_id,
            message_id: Some(message_id),
            // These rows are the corrections/overrides that motivated the candidate.
            evidence_kind: EvidenceKind::Override,
            weight: 1.0,
            summary,
            created_at: Timestamp::now(),
        });
    }
    (evidence, refs)
}

/// Build a filing-rule proposal (move messages from a sender domain to a folder) from a
/// cluster of repeated manual moves.
#[must_use]
pub fn filing_proposal(
    cluster: &FilingCluster,
    source: &str,
) -> (AgentProposal, Vec<RuleEvidence>) {
    let proposal_id = ProposalId::fresh();
    let draft = RuleDraft {
        kind: RuleKind::Action,
        scope: RuleScope::Domain,
        condition: sender_domain_eq(&cluster.sender_domain),
        effect: RuleEffect {
            move_to: Some(cluster.folder.as_str().to_owned()),
            ..RuleEffect::new()
        },
    };
    let (evidence, refs) = evidence_rows(
        &proposal_id,
        RuleKind::Action,
        EvidenceSourceKind::Filing,
        cluster.rows.iter().map(|r| {
            (
                r.id.clone(),
                r.message_id.clone(),
                format!("moved to {}", r.human_chosen_folder),
            )
        }),
    );
    let proposal = AgentProposal {
        id: proposal_id,
        proposal_type: ProposalKind::NewRule,
        status: ProposalStatus::PendingReview,
        title: format!("File {} mail to {}", cluster.sender_domain, cluster.folder),
        rationale: format!(
            "You moved {} messages from {} to {}.",
            cluster.rows.len(),
            cluster.sender_domain,
            cluster.folder
        ),
        risk_level: RiskLevel::Low,
        recommended_status: RuleStatus::ShadowMode,
        rule_draft: Some(draft),
        target_rule_kind: None,
        target_rule_id: None,
        workflow_draft: None,
        target_workflow_id: None,
        evidence_refs: refs,
        source_provider: source.to_owned(),
        created_at: Timestamp::now(),
        reviewed_at: None,
    };
    (proposal, evidence)
}

/// Build a classification-rule proposal (label mail from a sender domain) from a cluster of
/// repeated corrections to the same label.
#[must_use]
pub fn classification_proposal(
    cluster: &ClassificationCluster,
    source: &str,
) -> (AgentProposal, Vec<RuleEvidence>) {
    let proposal_id = ProposalId::fresh();
    let draft = RuleDraft {
        kind: RuleKind::Classification,
        scope: RuleScope::Domain,
        condition: sender_domain_eq(&cluster.sender_domain),
        effect: RuleEffect {
            set_labels: vec![cluster.label.clone()],
            ..RuleEffect::new()
        },
    };
    let (evidence, refs) = evidence_rows(
        &proposal_id,
        RuleKind::Classification,
        EvidenceSourceKind::Classification,
        cluster.rows.iter().map(|r| {
            (
                r.id.clone(),
                r.message_id.clone(),
                format!("corrected to {}", r.human_label),
            )
        }),
    );
    let proposal = AgentProposal {
        id: proposal_id,
        proposal_type: ProposalKind::NewRule,
        status: ProposalStatus::PendingReview,
        title: format!("Label {} mail as {}", cluster.sender_domain, cluster.label),
        rationale: format!(
            "You corrected {} messages from {} to {}.",
            cluster.rows.len(),
            cluster.sender_domain,
            cluster.label
        ),
        // A classification correction (spam/phishing) is higher-stakes than a filing move.
        risk_level: RiskLevel::Medium,
        recommended_status: RuleStatus::ShadowMode,
        rule_draft: Some(draft),
        target_rule_kind: None,
        target_rule_id: None,
        workflow_draft: None,
        target_workflow_id: None,
        evidence_refs: refs,
        source_provider: source.to_owned(),
        created_at: Timestamp::now(),
        reviewed_at: None,
    };
    (proposal, evidence)
}

/// Build a `new_workflow` proposal: a candidate follow-up cadence justified by repeated
/// manual follow-ups (the `followup_feedback` rows). The proposal recommends `shadow_mode`
/// — the curator may propose, but a human must review/activate, and every surfaced step is
/// review-required regardless. The risk band comes from the draft (`medium` by default).
#[must_use]
pub fn workflow_proposal(
    draft: WorkflowDraft,
    title: String,
    rationale: String,
    evidence_rows: &[FollowUpFeedbackRow],
    source: &str,
) -> AgentProposal {
    let risk_level = draft.risk_level;
    let evidence_refs = evidence_rows
        .iter()
        .map(|row| EvidenceRef {
            kind: EvidenceSourceKind::FollowUp,
            id: row.id.clone(),
        })
        .collect();
    AgentProposal {
        id: ProposalId::fresh(),
        proposal_type: ProposalKind::NewWorkflow,
        status: ProposalStatus::PendingReview,
        title,
        rationale,
        risk_level,
        // Cautious: a workflow is shadow-tested before any activation, never created active.
        recommended_status: RuleStatus::ShadowMode,
        rule_draft: None,
        target_rule_kind: None,
        target_rule_id: None,
        workflow_draft: Some(draft),
        target_workflow_id: None,
        evidence_refs,
        source_provider: source.to_owned(),
        created_at: Timestamp::now(),
        reviewed_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::features::{FeatureValue, FeatureVector};
    use mailmate_common::feedback::{ClassificationFeedback, FeedbackPolarity};
    use mailmate_common::feedback::{ClassificationFeedbackRow, FilingFeedbackRow};
    use mailmate_common::feedback::{FilingFeedback, PinnedVersions};
    use mailmate_common::ids::{FolderId, MessageId};

    fn filing_cluster() -> FilingCluster {
        let row = |folder: &str| FilingFeedbackRow {
            id: FilingFeedback::fresh_id(),
            message_id: MessageId::fresh(),
            pinned_versions: PinnedVersions::default(),
            sender_domain: Some("stripe.com".to_owned()),
            ai_suggested_folder: None,
            human_chosen_folder: FolderId::from(folder),
            basis: None,
            matched_rule_id: None,
            polarity: FeedbackPolarity::Negative,
            created_at: Timestamp::now(),
        };
        FilingCluster {
            sender_domain: "stripe.com".to_owned(),
            folder: FolderId::from("Receipts"),
            rows: vec![row("Receipts"), row("Receipts"), row("Receipts")],
        }
    }

    fn classification_cluster() -> ClassificationCluster {
        let mut salient = FeatureVector::new();
        salient.insert("sender_domain", FeatureValue::Text("paypa1.com".to_owned()));
        let row = ClassificationFeedbackRow {
            id: ClassificationFeedback::fresh_id(),
            message_id: MessageId::fresh(),
            pinned_versions: PinnedVersions::default(),
            ai_label: Some("not_junk".to_owned()),
            ai_score: None,
            ai_rationale: None,
            human_label: "phishing".to_owned(),
            human_reason_code: None,
            human_reason_text: None,
            salient_features: salient,
            polarity: FeedbackPolarity::Negative,
            created_at: Timestamp::now(),
        };
        ClassificationCluster {
            sender_domain: "paypa1.com".to_owned(),
            label: "phishing".to_owned(),
            rows: vec![row.clone(), row],
        }
    }

    #[test]
    fn filing_proposal_is_a_deterministic_action_draft_with_linked_evidence() {
        let (proposal, evidence) = filing_proposal(&filing_cluster(), "learning-engine");
        assert_eq!(proposal.proposal_type, ProposalKind::NewRule);
        assert_eq!(proposal.recommended_status, RuleStatus::ShadowMode);
        assert_eq!(proposal.risk_level, RiskLevel::Low);
        let draft = proposal.rule_draft.as_ref().unwrap();
        assert_eq!(draft.kind, RuleKind::Action);
        assert_eq!(draft.effect.move_to.as_deref(), Some("Receipts"));
        // Every evidence row links back to this proposal and a supporting feedback row.
        assert_eq!(evidence.len(), 3);
        assert_eq!(proposal.evidence_refs.len(), 3);
        assert!(evidence
            .iter()
            .all(|e| e.proposal_id.as_ref() == Some(&proposal.id)));
        assert!(evidence
            .iter()
            .all(|e| e.evidence_kind == EvidenceKind::Override));
    }

    #[test]
    fn classification_proposal_sets_labels_and_is_medium_risk() {
        let (proposal, evidence) =
            classification_proposal(&classification_cluster(), "learning-engine");
        let draft = proposal.rule_draft.as_ref().unwrap();
        assert_eq!(draft.kind, RuleKind::Classification);
        assert_eq!(draft.effect.set_labels, vec!["phishing".to_owned()]);
        assert_eq!(proposal.risk_level, RiskLevel::Medium);
        assert_eq!(evidence.len(), 2);
        assert!(proposal.title.contains("phishing"));
    }

    #[test]
    fn workflow_proposal_carries_a_draft_and_recommends_shadow_never_active() {
        use mailmate_common::feedback::{
            FollowUpFeedback, FollowUpFeedbackRow, FollowUpOutcome, PinnedVersions,
        };
        use mailmate_common::ids::{PipelineItemId, WorkflowInstanceId};
        use mailmate_common::pipeline::ItemType;
        use mailmate_common::workflow::{ExitCondition, FollowUpStep, Staleness, WorkflowAnchor};

        let evidence_row = FollowUpFeedbackRow {
            id: FollowUpFeedback::fresh_id(),
            workflow_instance_id: WorkflowInstanceId::from("wfi_1"),
            pipeline_item_id: PipelineItemId::from("pli_1"),
            step_index: 0,
            draft_id: None,
            pinned_versions: PinnedVersions::default(),
            ai_scheduled_offset_days: 3,
            actual_offset_days: Some(3),
            reply_received_before_step: false,
            reply_latency_days: None,
            outcome: FollowUpOutcome::ManualFollowupOffCadence,
            coalesced_from: vec![],
            human_reason_code: None,
            human_reason_text: None,
            polarity: FeedbackPolarity::Positive,
            created_at: Timestamp::now(),
        };
        let draft = WorkflowDraft {
            stable_name: "standard-quote-follow-up".to_owned(),
            scope: RuleScope::Global,
            applies_to_item_type: ItemType::Quote,
            anchor: WorkflowAnchor::QuoteSentAt,
            steps: vec![FollowUpStep {
                step_index: 0,
                offset_days: 3,
                draft_intent: "gentle_check_in".to_owned(),
                prompt_template_ref: None,
                forbidden_commitments: vec!["prices".to_owned()],
            }],
            exit_conditions: vec![ExitCondition::ReplyReceived],
            staleness: Staleness::default(),
            risk_level: RiskLevel::Medium,
        };
        let proposal = workflow_proposal(
            draft,
            "Standard quote follow-up: 3 / 7 / 14".to_owned(),
            "You manually followed up on 6 quotes at ~3 and ~7 days.".to_owned(),
            std::slice::from_ref(&evidence_row),
            "learning-engine",
        );
        assert_eq!(proposal.proposal_type, ProposalKind::NewWorkflow);
        assert_eq!(proposal.recommended_status, RuleStatus::ShadowMode);
        assert!(proposal.rule_draft.is_none());
        assert!(proposal.workflow_draft.is_some());
        assert_eq!(proposal.evidence_refs.len(), 1);
        assert_eq!(proposal.evidence_refs[0].kind, EvidenceSourceKind::FollowUp);
    }
}
