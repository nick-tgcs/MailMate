//! Evidence aggregation: turn per-task feedback rows into derived [`RuleEvidence`] items,
//! and cluster them into the candidate groups a proposal is built from.
//!
//! Pure functions — same rows in, same evidence/clusters out — so a proposal pass is
//! replayable. The clustering keys are exactly the "same domain → same folder" /
//! "same label, same domain" patterns the proposal thresholds count.

use std::collections::BTreeMap;

use mailmate_common::evidence::{EvidenceKind, EvidenceSourceKind, RuleEvidence};
use mailmate_common::feedback::{ClassificationFeedbackRow, FeedbackPolarity, FilingFeedbackRow};
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

/// The sender domain a classification row keys on, read from its salient features.
#[must_use]
pub fn classification_sender_domain(row: &ClassificationFeedbackRow) -> Option<String> {
    match row.salient_features.get("sender_domain") {
        Some(mailmate_common::features::FeatureValue::Text(domain)) => Some(domain.clone()),
        _ => None,
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

/// A group of classification corrections with the same corrected label and sender domain.
#[derive(Clone, Debug)]
pub struct ClassificationCluster {
    /// The sender domain the corrections share.
    pub sender_domain: String,
    /// The corrected label they all assert.
    pub label: String,
    /// The supporting rows.
    pub rows: Vec<ClassificationFeedbackRow>,
}

/// Cluster classification **corrections** (negative-polarity rows) by (sender domain,
/// corrected label). Reinforcements and rows without a sender domain are dropped. Returns
/// clusters in deterministic (domain, label) order.
#[must_use]
pub fn cluster_classification(rows: Vec<ClassificationFeedbackRow>) -> Vec<ClassificationCluster> {
    let mut groups: BTreeMap<(String, String), Vec<ClassificationFeedbackRow>> = BTreeMap::new();
    for row in rows {
        if row.polarity != FeedbackPolarity::Negative {
            continue;
        }
        let Some(domain) = classification_sender_domain(&row) else {
            continue;
        };
        let label = row.human_label.clone();
        groups.entry((domain, label)).or_default().push(row);
    }
    groups
        .into_iter()
        .map(|((sender_domain, label), rows)| ClassificationCluster {
            sender_domain,
            label,
            rows,
        })
        .collect()
}

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
    fn classification_clusters_only_negative_corrections() {
        let rows = vec![
            classification(Some("paypa1.com"), "phishing", FeedbackPolarity::Negative),
            classification(Some("paypa1.com"), "phishing", FeedbackPolarity::Negative),
            // A reinforcement (positive) does not feed a correction cluster.
            classification(Some("paypa1.com"), "phishing", FeedbackPolarity::Positive),
            // No sender domain → not clusterable.
            classification(None, "phishing", FeedbackPolarity::Negative),
        ];
        let clusters = cluster_classification(rows);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].label, "phishing");
        assert_eq!(clusters[0].sender_domain, "paypa1.com");
        assert_eq!(
            clusters[0].rows.len(),
            2,
            "only the two negative corrections"
        );
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
}
