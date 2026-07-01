//! Evidence aggregation: turn per-task feedback rows into derived [`RuleEvidence`] items,
//! and cluster them into the candidate groups a proposal is built from.
//!
//! Pure functions — same rows in, same evidence/clusters out — so a proposal pass is
//! replayable. The clustering keys are exactly the "same domain → same folder" /
//! "same label, same domain" patterns the proposal thresholds count.

use std::collections::BTreeMap;

use mailmate_common::evidence::{EvidenceKind, EvidenceSourceKind, RuleEvidence};
use mailmate_common::feedback::{
    ClassificationFeedbackRow, FeedbackPolarity, FilingFeedbackRow, FollowUpFeedbackRow,
};
use mailmate_common::ids::{EvidenceId, FolderId};

/// The intrinsic evidence kind of a captured row: an accepted prediction reinforces
/// (`Positive`); a correction is an `Override` of the AI. (A counterexample is supplied by
/// a safety pass, not derived here.)
#[must_use]
pub fn polarity_to_kind(polarity: FeedbackPolarity) -> EvidenceKind {
    match polarity {
        FeedbackPolarity::Positive => EvidenceKind::Positive,
        FeedbackPolarity::Negative => EvidenceKind::Override,
    }
}

/// Derive an evidence item from a filing-feedback row.
#[must_use]
pub fn evidence_from_filing(row: &FilingFeedbackRow) -> RuleEvidence {
    RuleEvidence {
        id: EvidenceId::fresh(),
        rule_kind: None,
        rule_id: None,
        proposal_id: None,
        source_kind: EvidenceSourceKind::Filing,
        source_id: row.id.clone(),
        message_id: Some(row.message_id.clone()),
        evidence_kind: polarity_to_kind(row.polarity),
        weight: 1.0,
        summary: format!("filed to {}", row.human_chosen_folder),
        created_at: row.created_at,
    }
}

/// Derive an evidence item from a classification-feedback row.
#[must_use]
pub fn evidence_from_classification(row: &ClassificationFeedbackRow) -> RuleEvidence {
    RuleEvidence {
        id: EvidenceId::fresh(),
        rule_kind: None,
        rule_id: None,
        proposal_id: None,
        source_kind: EvidenceSourceKind::Classification,
        source_id: row.id.clone(),
        message_id: Some(row.message_id.clone()),
        evidence_kind: polarity_to_kind(row.polarity),
        weight: 1.0,
        summary: format!("labeled {}", row.human_label),
        created_at: row.created_at,
    }
}

/// Derive an evidence item from a follow-up-feedback row. A follow-up step is time-triggered
/// and has no message, so `message_id` is `None`; the cadence signal (was the timing/decision
/// to follow up at all good?) feeds the curator's workflow proposals.
#[must_use]
pub fn evidence_from_followup(row: &FollowUpFeedbackRow) -> RuleEvidence {
    RuleEvidence {
        id: EvidenceId::fresh(),
        rule_kind: None,
        rule_id: None,
        proposal_id: None,
        source_kind: EvidenceSourceKind::FollowUp,
        source_id: row.id.clone(),
        message_id: None,
        evidence_kind: polarity_to_kind(row.polarity),
        weight: 1.0,
        summary: format!("followup step {} {}", row.step_index, row.outcome.as_str()),
        created_at: row.created_at,
    }
}

/// A group of filing moves with the same sender domain → same folder.
#[derive(Clone, Debug)]
pub struct FilingCluster {
    /// The sender domain the moves share.
    pub sender_domain: String,
    /// The folder they were all moved to.
    pub folder: FolderId,
    /// The supporting rows.
    pub rows: Vec<FilingFeedbackRow>,
}

/// Cluster filing rows by (sender domain, chosen folder). Rows without a sender domain are
/// not clusterable (the rule would have nothing deterministic to key on) and are dropped.
/// Returns clusters in deterministic (domain, folder) order.
#[must_use]
pub fn cluster_filing(rows: Vec<FilingFeedbackRow>) -> Vec<FilingCluster> {
    let mut groups: BTreeMap<(String, String), Vec<FilingFeedbackRow>> = BTreeMap::new();
    for row in rows {
        let Some(domain) = row.sender_domain.clone() else {
            continue;
        };
        let folder = row.human_chosen_folder.as_str().to_owned();
        groups.entry((domain, folder)).or_default().push(row);
    }
    groups
        .into_iter()
        .map(|((sender_domain, folder), rows)| FilingCluster {
            sender_domain,
            folder: FolderId::from(folder),
            rows,
        })
        .collect()
}

// Classification corrections are no longer clustered by sender domain here: Phase 7's
// `induction::cluster_by_effect` clusters them by effect (the corrected label) and *induces* a
// multi-aspect condition from the features they share. The old single-domain clustering was
// removed when that path went live, so there is only one way to turn corrections into a rule.

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::features::{FeatureValue, FeatureVector};
    use mailmate_common::feedback::{ClassificationFeedback, FilingFeedback, PinnedVersions};
    use mailmate_common::ids::MessageId;
    use mailmate_common::time::Timestamp;

    fn filing(domain: Option<&str>, folder: &str, polarity: FeedbackPolarity) -> FilingFeedbackRow {
        FilingFeedbackRow {
            id: FilingFeedback::fresh_id(),
            message_id: MessageId::fresh(),
            pinned_versions: PinnedVersions::default(),
            sender_domain: domain.map(str::to_owned),
            ai_suggested_folder: None,
            human_chosen_folder: FolderId::from(folder),
            basis: Some("domain".to_owned()),
            matched_rule_id: None,
            polarity,
            created_at: Timestamp::now(),
        }
    }

    fn classification(
        domain: Option<&str>,
        label: &str,
        polarity: FeedbackPolarity,
    ) -> ClassificationFeedbackRow {
        let mut salient = FeatureVector::new();
        if let Some(d) = domain {
            salient.insert("sender_domain", FeatureValue::Text(d.to_owned()));
        }
        ClassificationFeedbackRow {
            id: ClassificationFeedback::fresh_id(),
            message_id: MessageId::fresh(),
            pinned_versions: PinnedVersions::default(),
            ai_label: Some("not_junk".to_owned()),
            ai_score: None,
            ai_rationale: None,
            human_label: label.to_owned(),
            human_reason_code: None,
            human_reason_text: None,
            salient_features: salient,
            polarity,
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn filing_clusters_by_domain_and_folder_dropping_domainless_rows() {
        let rows = vec![
            filing(Some("stripe.com"), "Receipts", FeedbackPolarity::Negative),
            filing(Some("stripe.com"), "Receipts", FeedbackPolarity::Negative),
            filing(Some("stripe.com"), "Other", FeedbackPolarity::Negative),
            filing(None, "Receipts", FeedbackPolarity::Negative),
        ];
        let clusters = cluster_filing(rows);
        assert_eq!(
            clusters.len(),
            2,
            "two (domain,folder) groups; domainless dropped"
        );
        // Deterministic order: ("stripe.com","Other") sorts before ("stripe.com","Receipts").
        assert_eq!(clusters[0].folder.as_str(), "Other");
        assert_eq!(clusters[1].folder.as_str(), "Receipts");
        assert_eq!(clusters[1].rows.len(), 2);
    }

    #[test]
    fn evidence_derivation_maps_polarity_to_kind() {
        let pos = evidence_from_filing(&filing(Some("a.com"), "F", FeedbackPolarity::Positive));
        assert_eq!(pos.evidence_kind, EvidenceKind::Positive);
        assert_eq!(pos.source_kind, EvidenceSourceKind::Filing);
        let neg = evidence_from_classification(&classification(
            Some("b.com"),
            "spam",
            FeedbackPolarity::Negative,
        ));
        assert_eq!(neg.evidence_kind, EvidenceKind::Override);
        assert!(neg.summary.contains("spam"));
    }

    #[test]
    fn followup_evidence_has_no_message_and_carries_the_cadence_signal() {
        use mailmate_common::feedback::{FollowUpFeedback, FollowUpFeedbackRow, FollowUpOutcome};
        use mailmate_common::ids::{PipelineItemId, WorkflowInstanceId};
        let row = FollowUpFeedbackRow {
            id: FollowUpFeedback::fresh_id(),
            workflow_instance_id: WorkflowInstanceId::from("wfi_1"),
            pipeline_item_id: PipelineItemId::from("pli_1"),
            step_index: 2,
            draft_id: None,
            pinned_versions: PinnedVersions::default(),
            ai_scheduled_offset_days: 14,
            actual_offset_days: None,
            reply_received_before_step: false,
            reply_latency_days: None,
            outcome: FollowUpOutcome::SurfacedForReview,
            coalesced_from: vec![],
            human_reason_code: None,
            human_reason_text: None,
            polarity: FeedbackPolarity::Positive,
            created_at: Timestamp::now(),
        };
        let evidence = evidence_from_followup(&row);
        assert_eq!(evidence.source_kind, EvidenceSourceKind::FollowUp);
        assert_eq!(evidence.message_id, None);
        assert_eq!(evidence.evidence_kind, EvidenceKind::Positive);
        assert!(evidence.summary.contains("surfaced_for_review"));
    }
}
