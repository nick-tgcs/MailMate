//! [`DefaultLearningEngine`]: the [`LearningEngine`] port over the storage repositories.
//!
//! It captures corrections into their single-owner feedback table, records cross-cutting
//! provenance into the audit timeline, derives evidence from the feedback rows, and runs a
//! proposal pass that clusters repeated behavior into reviewable candidate rules. It never
//! activates a rule — every proposal it emits is `pending_review` and recommends
//! `shadow_mode`; the human-review and back-test gates stand between a proposal and a live
//! rule.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;

use mailmate_common::actor::Actor;
use mailmate_common::audit::{event_type, AuditEntry, AuditQuery};
use mailmate_common::error::LearningError;
use mailmate_common::evidence::{EvidenceQuery, EvidenceSourceKind, RuleEvidence};
use mailmate_common::feedback::{
    ClassificationFeedback, ClassificationFeedbackQuery, FilingFeedback, FilingFeedbackQuery,
    FilingFeedbackRow, TaskFeedback,
};
use mailmate_common::ids::{AuditId, FeedbackId, RuleId};
use mailmate_common::proposal::{AgentProposal, ProposalStatus, ProposalTrigger};
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::evaluation::ConflictSeverity;
use mailmate_common::rules::rule::{RiskLevel, RuleKind, RuleScope, RuleStatus};
use mailmate_rules::conflict::detect_conflicts;
use mailmate_ports::clock::Clock;
use mailmate_ports::learning_engine::LearningEngine;
use mailmate_ports::storage::audit::AuditRepository;
use mailmate_ports::storage::feedback::FeedbackRepository;
use mailmate_ports::storage::proposals::ProposalRepository;
use mailmate_ports::storage::rules::RuleRepository;

use mailmate_common::features::FeatureVector;
use mailmate_common::proposal::BackTest;
use mailmate_common::time::Timestamp;

use crate::decay::{
    assess_decay, assess_staleness, retire_proposal, stale_retire_proposal, DecayThresholds,
};
use crate::evidence::{cluster_filing, evidence_from_classification, evidence_from_filing};
use crate::induction::{cluster_by_effect, induce_condition};
use crate::proposals::{classification_proposal_induced, filing_proposal, vip_proposal};
use crate::shadow::{back_test, HistoricalExample};

/// Every rule scope a retire pass scans for decayed active rules.
const ALL_SCOPES: [RuleScope; 5] = [
    RuleScope::Global,
    RuleScope::Account,
    RuleScope::Folder,
    RuleScope::Sender,
    RuleScope::Domain,
];

/// The default source label stamped on proposals and audit entries this engine emits.
pub const DEFAULT_SOURCE: &str = "learning-engine";

/// The default learning engine, composing the per-task feedback repositories, the audit
/// timeline, and the proposal store.
pub struct DefaultLearningEngine {
    classification_feedback: Arc<dyn FeedbackRepository<ClassificationFeedback>>,
    filing_feedback: Arc<dyn FeedbackRepository<FilingFeedback>>,
    audit: Arc<dyn AuditRepository>,
    proposals: Arc<dyn ProposalRepository>,
    /// The active-rule snapshot the decay pass scans. Optional so the existing constructors and
    /// tests stand unchanged; when absent, the retire pass is simply a no-op.
    rules: Option<Arc<dyn RuleRepository>>,
    /// The clock the *staleness* half of the decay pass measures idle time against. Optional like
    /// `rules`: without it, the undo-decay path still runs but the no-fires-in-M-days path is a
    /// no-op (we never guess "now").
    clock: Option<Arc<dyn Clock>>,
    source_label: String,
}

impl DefaultLearningEngine {
    /// Build an engine over the given repositories, using [`DEFAULT_SOURCE`] as the source
    /// label.
    #[must_use]
    pub fn new(
        classification_feedback: Arc<dyn FeedbackRepository<ClassificationFeedback>>,
        filing_feedback: Arc<dyn FeedbackRepository<FilingFeedback>>,
        audit: Arc<dyn AuditRepository>,
        proposals: Arc<dyn ProposalRepository>,
    ) -> Self {
        Self {
            classification_feedback,
            filing_feedback,
            audit,
            proposals,
            rules: None,
            clock: None,
            source_label: DEFAULT_SOURCE.to_owned(),
        }
    }

    /// Override the source label stamped on emitted proposals/audit entries.
    #[must_use]
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source_label = source.into();
        self
    }

    /// Give the engine the rule snapshot it scans for decay, enabling the human-gated retire
    /// pass. Without it, [`propose_candidates`](Self::propose_candidates) emits only the
    /// new-rule proposals (the retire pass is a no-op).
    #[must_use]
    pub fn with_rules(mut self, rules: Arc<dyn RuleRepository>) -> Self {
        self.rules = Some(rules);
        self
    }

    /// Give the engine a clock, enabling the **staleness** half of the decay pass (a rule that
    /// hasn't fired in `max_idle_days`). Without it, only the undo-decay path runs.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// How many audit rows of `event_type` are stamped against `rule_id`.
    async fn audit_count(&self, event_type: &str, rule_id: &RuleId) -> Result<usize, LearningError> {
        let rows = self
            .audit
            .query(AuditQuery {
                event_type: Some(event_type.to_owned()),
                rule_id: Some(rule_id.clone()),
                message_id: None,
                limit: None,
            })
            .await?;
        Ok(rows.len())
    }

    /// The timestamp of the most recent `event_type` row stamped against `rule_id`, or `None` if
    /// there is none. The audit query returns newest-first, so a `limit` of 1 yields the latest.
    async fn latest_audit_at(
        &self,
        event_type: &str,
        rule_id: &RuleId,
    ) -> Result<Option<Timestamp>, LearningError> {
        let rows = self
            .audit
            .query(AuditQuery {
                event_type: Some(event_type.to_owned()),
                rule_id: Some(rule_id.clone()),
                message_id: None,
                limit: Some(1),
            })
            .await?;
        Ok(rows.first().map(|entry| entry.created_at))
    }

    /// Scan active rules and surface a human-gated [`RetireRule`](mailmate_common::proposal::ProposalKind::RetireRule)
    /// proposal for any whose track record has decayed (a high undo count/rate). A no-op without a
    /// rule repository. Each retire is deduped by signature (its target rule), so a recurring pass
    /// proposes a given rule's retirement once. Never retires — only proposes.
    async fn propose_retirements(
        &self,
        seen: &mut HashSet<String>,
        out: &mut Vec<AgentProposal>,
    ) -> Result<(), LearningError> {
        let Some(rules) = self.rules.as_ref() else {
            return Ok(());
        };
        let thresholds = DecayThresholds::default();
        // A rule lives at exactly one scope, but we scan all of them — `considered` guards against
        // assessing the same rule twice if a backend ever returned it under two scopes.
        let mut considered: HashSet<RuleId> = HashSet::new();
        for kind in [RuleKind::Action, RuleKind::Classification] {
            for scope in ALL_SCOPES {
                for rule in rules.get_active_rules(kind, scope).await? {
                    if !considered.insert(rule.rule_id.clone()) {
                        continue;
                    }
                    let undos = self.audit_count(event_type::ACTION_UNDONE, &rule.rule_id).await?;
                    let applied = self.audit_count(event_type::ACTION_APPLIED, &rule.rule_id).await?;
                    // The apply path now stamps `action_applied` with the authoring rule_id, so this
                    // count is the real per-rule fires denominator. We still map 0 to `None` rather
                    // than a 0 denominator: a rule with no recorded fires (freshly activated, or
                    // fires predating the stamping) has no trustworthy rate, so the count fallback
                    // applies instead of a divide-by-zero.
                    let fires = (applied > 0).then_some(applied);
                    // The undo signal is the more urgent (the rule is actively doing harm), so assess
                    // it first. A rule that isn't being undone but has gone quiet is *stale*. Both
                    // surface a RetireRule for the same target, so the signature dedup guarantees at
                    // most one retire per rule per pass — the undo rationale wins when both apply.
                    let proposal = if let Some(verdict) = assess_decay(undos, fires, thresholds) {
                        Some(retire_proposal(&rule, verdict, &self.source_label))
                    } else if let Some(clock) = self.clock.as_ref() {
                        let last_fire = self
                            .latest_audit_at(event_type::ACTION_APPLIED, &rule.rule_id)
                            .await?;
                        let activated_at = self
                            .latest_audit_at(event_type::RULE_ACTIVATED, &rule.rule_id)
                            .await?;
                        assess_staleness(last_fire, activated_at, clock.now(), thresholds.max_idle_days)
                            .map(|verdict| stale_retire_proposal(&rule, verdict, &self.source_label))
                    } else {
                        None
                    };
                    let Some(proposal) = proposal else {
                        continue;
                    };
                    if seen.insert(proposal_signature(&proposal)) {
                        self.persist_proposal(&proposal, Vec::new()).await?;
                        out.push(proposal);
                    }
                }
            }
        }
        Ok(())
    }

    /// Force a candidate proposal to human review when its rule conflicts with an existing **active**
    /// rule — exact contradiction, subsumption, or co-match overlap with a differing effect (Phase 7
    /// overlap/subsumption detection). The detected conflicts ride on the proposal so the Review card
    /// renders the "conflicts" measure, and `recommended_status` becomes
    /// [`PendingHumanReview`](RuleStatus::PendingHumanReview): on acceptance the rule lands in a
    /// non-firing review state, so the human must adjudicate the overlap before it can ever fire
    /// (the §3.6 "risky/conflicting proposals are forced to human review" invariant). A no-op
    /// without a rule snapshot, or for a proposal that carries no candidate draft (retire/stale).
    async fn apply_conflict_gate(&self, proposal: &mut AgentProposal) -> Result<(), LearningError> {
        let Some(rules) = self.rules.as_ref() else {
            return Ok(());
        };
        let Some(draft) = proposal.rule_draft.as_ref() else {
            return Ok(());
        };
        // The live rules to check against: every active rule of the candidate's kind (detect_conflicts
        // filters by scope itself). A conflict with a shadow/disabled/retired rule is not a live one.
        let mut existing = Vec::new();
        for scope in ALL_SCOPES {
            existing.extend(rules.get_active_rules(draft.kind, scope).await?);
        }
        let conflicts = detect_conflicts(draft, &existing);
        if conflicts.is_empty() {
            return Ok(());
        }
        // A strict contradiction is High; a softer overlap raises a cautious candidate to at least
        // Medium. Either way the proposal is forced to PendingHumanReview, never auto-shadowed.
        let high = conflicts.iter().any(|c| c.severity == ConflictSeverity::High);
        proposal.risk_level = if high {
            RiskLevel::High
        } else {
            proposal.risk_level.max(RiskLevel::Medium)
        };
        proposal.recommended_status = RuleStatus::PendingHumanReview;
        proposal.conflicts = conflicts;
        Ok(())
    }

    /// Mine the outbound (`mail_sent`) audit trail and surface a human-gated VIP/priority proposal
    /// for any recipient domain the user has emailed at least `min_sends` times (learn-from-Sent):
    /// people you repeatedly write to are people whose inbound mail matters. Like every proposal it
    /// only surfaces — recommends `shadow_mode`, never activates — and is deduped by signature so a
    /// given domain's VIP rule is proposed once. Runs whenever there is outbound evidence (it needs
    /// no feedback table); a no-op when nobody has been emailed `min_sends` times.
    async fn propose_vip_rules(
        &self,
        min_sends: usize,
        seen: &mut HashSet<String>,
        out: &mut Vec<AgentProposal>,
    ) -> Result<(), LearningError> {
        let rows = self
            .audit
            .query(AuditQuery {
                event_type: Some(event_type::MAIL_SENT.to_owned()),
                rule_id: None,
                message_id: None,
                limit: None,
            })
            .await?;
        // Count sends per recipient domain (one `mail_sent` row per recipient, its domain in the
        // payload). Deterministic `(domain → count)` order so a pass is replayable.
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for row in rows {
            if let Some(domain) = row
                .payload
                .get("recipient_domain")
                .and_then(serde_json::Value::as_str)
                .filter(|d| !d.is_empty())
            {
                *counts.entry(domain.to_owned()).or_default() += 1;
            }
        }
        for (domain, count) in counts {
            if count < min_sends {
                continue;
            }
            let mut proposal = vip_proposal(&domain, count, &self.source_label);
            // A VIP rule can overlap an existing active rule too — force review if so.
            self.apply_conflict_gate(&mut proposal).await?;
            if seen.insert(proposal_signature(&proposal)) {
                self.persist_proposal(&proposal, Vec::new()).await?;
                out.push(proposal);
            }
        }
        Ok(())
    }

    /// Persist a proposal with its evidence and record the proposal in the audit timeline.
    async fn persist_proposal(
        &self,
        proposal: &AgentProposal,
        evidence: Vec<RuleEvidence>,
    ) -> Result<(), LearningError> {
        self.proposals.save(proposal.clone(), evidence).await?;
        let entry = AuditEntry::new(event_type::RULE_PROPOSED, Actor::Ai)
            .with_proposal(proposal.id.clone())
            .with_payload(serde_json::json!({
                "title": proposal.title,
                "proposal_type": proposal.proposal_type.as_str(),
                "recommended_status": proposal.recommended_status.as_str(),
            }));
        self.audit.append(entry).await?;
        Ok(())
    }

    /// The signatures of every proposal already on record — the dedup set a fresh pass checks
    /// against. It spans the pending/shadowing states and the terminal accepted/rejected
    /// dispositions, so a cluster that has already been proposed (whatever its fate) is not
    /// proposed again — in particular, a rejected cluster is not re-proposed without new
    /// evidence (the invariant on [`ProposalStatus::Rejected`]).
    async fn existing_signatures(&self) -> Result<HashSet<String>, LearningError> {
        let mut signatures = HashSet::new();
        for status in [
            ProposalStatus::Draft,
            ProposalStatus::PendingReview,
            ProposalStatus::Shadowing,
            ProposalStatus::Accepted,
            ProposalStatus::Rejected,
        ] {
            for proposal in self.proposals.list_by_status(status).await? {
                signatures.insert(proposal_signature(&proposal));
            }
        }
        Ok(signatures)
    }
}

/// A deterministic identity for *what a proposal would change* — its kind plus the canonical
/// JSON of its candidate rule/workflow draft. Two passes over the same recurring pattern build
/// byte-identical drafts, hence the same signature; this is the cross-pass dedup key that makes
/// [`DefaultLearningEngine::propose_candidates`] idempotent (re-running adds nothing new).
fn proposal_signature(proposal: &AgentProposal) -> String {
    let rule = serde_json::to_string(&proposal.rule_draft).unwrap_or_default();
    let workflow = serde_json::to_string(&proposal.workflow_draft).unwrap_or_default();
    // The target ids matter for proposals that carry no draft (retire/refine reference an
    // existing rule/workflow): without them every `retire_rule` proposal would share one
    // signature and collide, so a second decayed rule would be silently de-duped away.
    let target_rule = serde_json::to_string(&proposal.target_rule_id).unwrap_or_default();
    let target_workflow = serde_json::to_string(&proposal.target_workflow_id).unwrap_or_default();
    // The title distinguishes two retire proposals for the SAME rule that arise from DIFFERENT
    // decay reasons (the undo-driven "…you keep undoing" vs the time-driven "…gone quiet"): without
    // it they share a signature, so a rejected undo-retire would permanently suppress a later, valid
    // stale-retire for that rule. Titles are deterministic per pattern, so idempotency holds.
    format!(
        "{}|{rule}|{workflow}|{target_rule}|{target_workflow}|{}",
        proposal.proposal_type.as_str(),
        proposal.title
    )
}

fn wants(filter: Option<EvidenceSourceKind>, kind: EvidenceSourceKind) -> bool {
    filter.is_none() || filter == Some(kind)
}

/// Reconstruct the crystallization back-test history per sender domain from filing rows: every
/// move the user made *from a domain* — to whatever folder — is one historical example (the
/// deterministic field environment plus the folder they actually chose). A candidate filing rule
/// is then back-tested against its domain's WHOLE history, not just the agreeing subset, so a
/// candidate that also matches mail the user filed elsewhere scores below the precision bar and
/// is withheld. Domainless rows have nothing deterministic to key on and are skipped (they are
/// dropped by clustering too). Ordering is deterministic (the rows arrive sorted by the
/// repository) so a back-test replays identically.
fn filing_history_by_domain(rows: &[FilingFeedbackRow]) -> BTreeMap<String, Vec<HistoricalExample>> {
    let mut by_domain: BTreeMap<String, Vec<HistoricalExample>> = BTreeMap::new();
    for row in rows {
        let Some(domain) = row.sender_domain.clone() else {
            continue;
        };
        let effect = RuleEffect {
            move_to: Some(row.human_chosen_folder.as_str().to_owned()),
            ..RuleEffect::new()
        };
        by_domain
            .entry(domain.clone())
            .or_default()
            .push(HistoricalExample::from_domain(&domain, effect));
    }
    by_domain
}

#[async_trait]
impl LearningEngine for DefaultLearningEngine {
    async fn record_feedback(&self, feedback: TaskFeedback) -> Result<FeedbackId, LearningError> {
        let id = match feedback {
            TaskFeedback::Classification(row) => self.classification_feedback.append(row).await?,
            TaskFeedback::Filing(row) => self.filing_feedback.append(row).await?,
        };
        Ok(id)
    }

    async fn record_audit(&self, entry: AuditEntry) -> Result<AuditId, LearningError> {
        Ok(self.audit.append(entry).await?)
    }

    async fn collect_evidence(
        &self,
        query: EvidenceQuery,
    ) -> Result<Vec<RuleEvidence>, LearningError> {
        let mut out = Vec::new();
        if wants(query.source_kind, EvidenceSourceKind::Classification) {
            let rows = self
                .classification_feedback
                .query(ClassificationFeedbackQuery {
                    message_id: query.message_id.clone(),
                    human_label: None,
                    limit: query.limit,
                })
                .await?;
            out.extend(rows.iter().map(evidence_from_classification));
        }
        if wants(query.source_kind, EvidenceSourceKind::Filing) {
            let rows = self
                .filing_feedback
                .query(FilingFeedbackQuery {
                    message_id: query.message_id.clone(),
                    human_chosen_folder: None,
                    limit: query.limit,
                })
                .await?;
            out.extend(rows.iter().map(evidence_from_filing));
        }
        if let Some(limit) = query.limit {
            out.truncate(limit);
        }
        Ok(out)
    }

    async fn propose_candidates(
        &self,
        trigger: ProposalTrigger,
    ) -> Result<Vec<AgentProposal>, LearningError> {
        let mut proposals = Vec::new();
        // Idempotency: a recurring cluster proposes ONCE. We dedup each candidate by its
        // deterministic signature against everything already proposed, so re-running a pass
        // (on every tick / launch) adds nothing new and never piles up duplicates. `seen` is
        // seeded from the store and then also collects this pass's emissions.
        let mut seen = self.existing_signatures().await?;

        if wants(trigger.source_kind, EvidenceSourceKind::Filing) {
            let rows = self
                .filing_feedback
                .query(FilingFeedbackQuery::default())
                .await?;
            // Reconstruct each domain's full move history BEFORE clustering consumes the rows, so
            // the back-test below can see contradicting moves (a domain filed to two folders), not
            // just the cluster's agreeing subset.
            let history = filing_history_by_domain(&rows);
            for cluster in cluster_filing(rows) {
                if cluster.rows.len() < trigger.thresholds.min_filing_moves {
                    continue;
                }
                let (mut proposal, evidence) = filing_proposal(&cluster, &self.source_label);
                // The crystallization gate (promotion-gate step 2): the candidate must reproduce
                // the user's past filings of this domain at the precision bar across the WHOLE
                // history — this is the first production caller of `back_test`. A candidate that
                // mis-files some of the domain's mail is withheld (never persisted, never nagged).
                let draft = proposal
                    .rule_draft
                    .as_ref()
                    .expect("a filing proposal always carries a rule draft");
                let domain_history = history
                    .get(&cluster.sender_domain)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let report = back_test(draft, domain_history).await?;
                if !report.is_eligible(
                    trigger.thresholds.filing_precision_bar,
                    trigger.thresholds.min_filing_moves,
                ) {
                    continue;
                }
                // Capture the gate's verdict ON the proposal so the Review card shows the exact
                // precision · support the candidate was admitted on (not a re-derived estimate).
                // `support` is `report.correct` — the past decisions the rule actually reproduces —
                // the SAME meaning the induced-classification path stamps (below), so the card's
                // "support N msgs" reads identically on both. For a filing candidate that cleared
                // the 0.9 bar `correct` and `fires` are near-equal, but `correct` is the honest one
                // (it never counts a decision the rule got wrong).
                proposal.back_test = Some(mailmate_common::proposal::BackTest {
                    precision: report.precision(),
                    support: report.correct,
                });
                // Force review if this filing rule conflicts with an existing active rule.
                self.apply_conflict_gate(&mut proposal).await?;
                if seen.insert(proposal_signature(&proposal)) {
                    self.persist_proposal(&proposal, evidence).await?;
                    proposals.push(proposal);
                }
            }
        }

        if wants(trigger.source_kind, EvidenceSourceKind::Classification) {
            let rows = self
                .classification_feedback
                .query(ClassificationFeedbackQuery::default())
                .await?;
            // 7a/7b: cluster by EFFECT (the corrected label), keeping domainless rows, then
            // INDUCE the condition from the features the cluster shares — back-tested against a
            // negative pool of every other captured correction so a false positive is visible.
            let clusters = cluster_by_effect(rows.clone());
            for cluster in &clusters {
                if cluster.rows.len() < trigger.thresholds.min_classification_corrections {
                    continue;
                }
                let positives: Vec<FeatureVector> = cluster
                    .rows
                    .iter()
                    .map(|r| r.salient_features.clone())
                    .collect();
                // The negative pool: every captured correction with a DIFFERENT label. A same-label
                // row is never a counterexample — the user applied this very label to it — so it
                // must be excluded regardless of account (keying on the (account,label) effect would
                // wrongly pull a same-label row from another account into the pool, where its empty
                // effect deflates the candidate's precision and withholds a good rule). Cross-account
                // *different-label* rows stay in: a candidate that mislabels them IS a real false
                // positive the precision bar should catch.
                let negatives: Vec<FeatureVector> = rows
                    .iter()
                    .filter(|r| r.human_label != cluster.label)
                    .map(|r| r.salient_features.clone())
                    .collect();

                let bar = trigger.thresholds.classification_precision_bar;
                let floor = trigger.thresholds.min_classification_corrections;
                let Some((condition, report)) =
                    induce_condition(&positives, &negatives, &cluster.label, floor, bar).await?
                else {
                    continue;
                };
                // Gate on precision ≥ bar AND positive support ≥ floor. The honest positive
                // support is `report.correct` (the cluster members reproduced); `support()` is
                // `fires`, which over this pool also counts negative-pool false positives.
                let precision_ok = report.precision().is_some_and(|p| p >= bar);
                if !precision_ok || report.correct < floor {
                    continue;
                }
                let (mut proposal, evidence) = classification_proposal_induced(
                    cluster.scope,
                    &cluster.label,
                    condition,
                    &cluster.rows,
                    &self.source_label,
                );
                // Stamp the gate's verdict: precision over the pool, support = positives
                // reproduced (never the FP-inflated `fires`).
                proposal.back_test = Some(BackTest {
                    precision: report.precision(),
                    support: report.correct,
                });
                // Force review if this induced rule subsumes/overlaps/contradicts an active rule.
                self.apply_conflict_gate(&mut proposal).await?;
                if seen.insert(proposal_signature(&proposal)) {
                    self.persist_proposal(&proposal, evidence).await?;
                    proposals.push(proposal);
                }
            }
        }

        // The decay and learn-from-Sent passes are whole-system signals, not tied to a feedback
        // SOURCE, so they run only on a full pass. A source-filtered trigger (e.g. "just filing")
        // asks for exactly that source and must not get retire/VIP proposals mixed in.
        if trigger.source_kind.is_none() {
            // Decay: surface human-gated retire proposals for active rules the user keeps undoing
            // (or that have gone stale).
            self.propose_retirements(&mut seen, &mut proposals).await?;

            // Learn-from-Sent: surface human-gated VIP/priority proposals for frequent outbound
            // contacts.
            self.propose_vip_rules(
                trigger.thresholds.min_outbound_sends,
                &mut seen,
                &mut proposals,
            )
            .await?;
        }

        Ok(proposals)
    }
}
