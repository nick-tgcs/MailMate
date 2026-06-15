//! [`AiRuleCurator`]: the [`RuleCurator`] port — the AI advisor over the explicit rule
//! system.
//!
//! Determinism-first: the curator is the **teacher**, never the executor. It assembles a
//! redacted context (the current rules, the recent feedback pattern, the open conflicts),
//! asks the provider's [`curate_rules`] task for changes, and turns each returned change into
//! a *reviewable* [`AgentProposal`] — `pending_review`, recommending shadow/human-review,
//! **never active**. Two safety clamps stand between the model and a live rule:
//!
//! 1. a risky proposal (`high`/`critical`) is forced to `pending_human_review`, whatever the
//!    model recommended;
//! 2. a candidate whose draft contradicts an existing rule (per the deterministic
//!    [`RuleEngine::detect_conflicts`]) is likewise forced to human review.
//!
//! Conflict *detection over live rules* is wholly deterministic (no model): a pairwise scan
//! records every contradictory pair into `rule_conflicts` for a human to resolve. Provider
//! failures and invalid responses are audited and drive no proposal.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use mailmate_ai::schemas::CuratorProposalDto;
use mailmate_ai::tasks::{curate_rules, CurateRulesInput};
use mailmate_common::actor::Actor;
use mailmate_common::audit::{event_type, AuditEntry};
use mailmate_common::conflict::RuleConflictRecord;
use mailmate_common::curator::{
    CuratorOperation, CuratorReport, CuratorRequest, FeedbackSummary, ThresholdSuggestion,
};
use mailmate_common::error::CuratorError;
use mailmate_common::evidence::{EvidenceQuery, EvidenceSourceKind, RuleEvidence};
use mailmate_common::ids::ProposalId;
use mailmate_common::proposal::{AgentProposal, ProposalKind, ProposalStatus};
use mailmate_common::rules::evaluation::{ConflictKind, ConflictSeverity};
use mailmate_common::rules::rule::{EvaluatableRule, RiskLevel, RuleKind, RuleScope, RuleStatus};
use mailmate_common::time::Timestamp;
use mailmate_ports::ai_provider::AiProvider;
use mailmate_ports::learning_engine::LearningEngine;
use mailmate_ports::rule_curator::RuleCurator;
use mailmate_ports::rule_engine::RuleEngine;
use mailmate_ports::storage::audit::AuditRepository;
use mailmate_ports::storage::conflicts::ConflictRepository;
use mailmate_ports::storage::proposals::ProposalRepository;
use mailmate_ports::storage::rules::RuleRepository;
use mailmate_rules::conflict::conditions_equivalent;

/// The default source label stamped on proposals/audit entries this curator emits.
pub const CURATOR_SOURCE: &str = "ai-curator";

/// Every rule scope the conflict scan and rule summary sweep.
const ALL_SCOPES: [RuleScope; 5] = [
    RuleScope::Global,
    RuleScope::Account,
    RuleScope::Folder,
    RuleScope::Sender,
    RuleScope::Domain,
];

/// The AI rule curator, composing the provider, the learning engine (for evidence), the
/// rule/proposal/conflict stores, and the deterministic rule engine (for conflict checks).
pub struct AiRuleCurator {
    provider: Arc<dyn AiProvider>,
    learning: Arc<dyn LearningEngine>,
    rules: Arc<dyn RuleRepository>,
    rule_engine: Arc<dyn RuleEngine>,
    proposals: Arc<dyn ProposalRepository>,
    conflicts: Arc<dyn ConflictRepository>,
    audit: Arc<dyn AuditRepository>,
    source_label: String,
}

impl AiRuleCurator {
    /// Build a curator over the given collaborators, using [`CURATOR_SOURCE`] as the label.
    #[must_use]
    pub fn new(
        provider: Arc<dyn AiProvider>,
        learning: Arc<dyn LearningEngine>,
        rules: Arc<dyn RuleRepository>,
        rule_engine: Arc<dyn RuleEngine>,
        proposals: Arc<dyn ProposalRepository>,
        conflicts: Arc<dyn ConflictRepository>,
        audit: Arc<dyn AuditRepository>,
    ) -> Self {
        Self {
            provider,
            learning,
            rules,
            rule_engine,
            proposals,
            conflicts,
            audit,
            source_label: CURATOR_SOURCE.to_owned(),
        }
    }

    /// Override the source label stamped on emitted proposals/audit entries.
    #[must_use]
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source_label = source.into();
        self
    }

    /// The rule kinds a request covers (both pipelines unless it pins one).
    fn kinds(request: &CuratorRequest) -> Vec<RuleKind> {
        match request.rule_kind {
            Some(kind) => vec![kind],
            None => vec![RuleKind::Classification, RuleKind::Action],
        }
    }

    /// A bounded, redacted summary of the current active+shadow rules the curator may change.
    async fn rules_summary(&self, kinds: &[RuleKind]) -> Result<String, CuratorError> {
        let mut lines = Vec::new();
        for &kind in kinds {
            for &scope in &ALL_SCOPES {
                let mut rules = self.rules.get_active_rules(kind, scope).await?;
                rules.extend(self.rules.get_shadow_rules(kind, scope).await?);
                for rule in &rules {
                    lines.push(format!(
                        "- {} [{}] {}/{}: {}",
                        rule.rule_id,
                        rule.status.as_str(),
                        kind.as_str(),
                        scope.as_str(),
                        effect_label(rule)
                    ));
                }
            }
        }
        Ok(if lines.is_empty() {
            "(no rules yet)".to_owned()
        } else {
            lines.join("\n")
        })
    }

    /// Map a validated curator DTO into a reviewable proposal, applying the safety clamps and
    /// the deterministic candidate-conflict check, then persist it and audit the emission.
    async fn persist_proposal(
        &self,
        dto: CuratorProposalDto,
    ) -> Result<AgentProposal, CuratorError> {
        let mut recommended_status = clamp_status(dto.risk_level, dto.recommended_status);
        let mut rationale = dto.rationale;

        // A candidate that contradicts an existing rule cannot be shadowed silently — it goes
        // to a human regardless of how cautious the model was.
        if let Some(draft) = &dto.rule_draft {
            let conflicts = self.rule_engine.detect_conflicts(draft.clone()).await?;
            if !conflicts.is_empty() {
                recommended_status = RuleStatus::PendingHumanReview;
                rationale = format!(
                    "{rationale} [conflicts with {} existing rule(s); needs human review]",
                    conflicts.len()
                );
            }
        }

        let proposal = AgentProposal {
            id: ProposalId::fresh(),
            proposal_type: dto.proposal_type,
            status: ProposalStatus::PendingReview,
            title: dto.title,
            rationale,
            risk_level: dto.risk_level,
            recommended_status,
            rule_draft: dto.rule_draft,
            target_rule_kind: dto.target_rule_kind,
            target_rule_id: dto.target_rule_id,
            evidence_refs: Vec::new(),
            source_provider: self.source_label.clone(),
            created_at: Timestamp::now(),
            reviewed_at: None,
        };
        self.proposals.save(proposal.clone(), Vec::new()).await?;
        let entry = AuditEntry::new(event_type::RULE_PROPOSED, Actor::Ai)
            .with_proposal(proposal.id.clone())
            .with_payload(json!({
                "title": proposal.title,
                "proposal_type": proposal.proposal_type.as_str(),
                "recommended_status": proposal.recommended_status.as_str(),
                "source": self.source_label,
            }));
        self.audit.append(entry).await?;
        Ok(proposal)
    }

    /// Deterministic conflict detection over LIVE rules: every same-kind, same-scope pair
    /// that shares a condition but contradicts in effect is recorded in `rule_conflicts`.
    async fn scan_conflicts(
        &self,
        kinds: &[RuleKind],
    ) -> Result<Vec<RuleConflictRecord>, CuratorError> {
        let mut out = Vec::new();
        for &kind in kinds {
            for &scope in &ALL_SCOPES {
                let rules = self.rules.get_active_rules(kind, scope).await?;
                for i in 0..rules.len() {
                    for j in (i + 1)..rules.len() {
                        let (a, b) = (&rules[i], &rules[j]);
                        if conditions_equivalent(&a.version.condition, &b.version.condition)
                            && a.version.effect.contradicts(&b.version.effect)
                        {
                            let record = RuleConflictRecord::new(
                                kind,
                                a.rule_id.clone(),
                                b.rule_id.clone(),
                                ConflictKind::ContradictoryEffect,
                                ConflictSeverity::High,
                                format!(
                                    "rules {} and {} share a condition but contradict in effect",
                                    a.rule_id, b.rule_id
                                ),
                            );
                            self.conflicts.append(record.clone()).await?;
                            let entry =
                                AuditEntry::new(event_type::RULE_CONFLICT_DETECTED, Actor::Ai)
                                    .with_rule(kind, a.rule_id.clone())
                                    .with_payload(json!({
                                        "conflict_id": record.id.as_str(),
                                        "rule_b": b.rule_id.as_str(),
                                    }));
                            self.audit.append(entry).await?;
                            out.push(record);
                        }
                    }
                }
            }
        }
        Ok(out)
    }
}

/// A compact `move=…`/`labels=…`/`junk=…` label for a rule's effect (for the prompt).
fn effect_label(rule: &EvaluatableRule) -> String {
    let effect = &rule.version.effect;
    if let Some(folder) = &effect.move_to {
        format!("move -> {folder}")
    } else if !effect.set_labels.is_empty() {
        format!("labels {:?}", effect.set_labels)
    } else if let Some(junk) = effect.mark_junk {
        format!("mark_junk {junk}")
    } else {
        "no-op".to_owned()
    }
}

/// The curator's safety clamp: a risky proposal always goes to human review; an `active`
/// recommendation (which the response validator already rejects) defensively degrades to
/// shadow; anything else keeps its recommended status.
fn clamp_status(risk: RiskLevel, recommended: RuleStatus) -> RuleStatus {
    if matches!(risk, RiskLevel::High | RiskLevel::Critical) {
        RuleStatus::PendingHumanReview
    } else if recommended == RuleStatus::Active {
        RuleStatus::ShadowMode
    } else {
        recommended
    }
}

/// Which curator operation a proposal kind belongs to (so a focused request keeps only the
/// proposal kinds it asked for).
fn operation_for(kind: ProposalKind) -> CuratorOperation {
    match kind {
        ProposalKind::NewRule => CuratorOperation::Propose,
        ProposalKind::RefineRule => CuratorOperation::Refine,
        ProposalKind::MergeRules => CuratorOperation::Merge,
        ProposalKind::SplitRule => CuratorOperation::Split,
        ProposalKind::RetireRule => CuratorOperation::DetectStale,
    }
}

/// The evidence source that contributed the most rows (the "primary" feedback signal), or
/// classification when there is no evidence yet.
fn dominant_source(evidence: &[RuleEvidence]) -> EvidenceSourceKind {
    let mut counts: std::collections::BTreeMap<&str, (usize, EvidenceSourceKind)> =
        std::collections::BTreeMap::new();
    for e in evidence {
        let entry = counts
            .entry(e.source_kind.as_str())
            .or_insert((0, e.source_kind));
        entry.0 += 1;
    }
    counts
        .into_values()
        .max_by_key(|(count, _)| *count)
        .map_or(EvidenceSourceKind::Classification, |(_, kind)| kind)
}

/// A short, model-free description of the recent feedback volume (for the prompt context).
fn feedback_summary_text(evidence: &[RuleEvidence]) -> String {
    if evidence.is_empty() {
        return "(no feedback captured yet)".to_owned();
    }
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for e in evidence {
        *counts.entry(e.source_kind.as_str()).or_insert(0) += 1;
    }
    let parts: Vec<String> = counts.iter().map(|(k, n)| format!("{k}={n}")).collect();
    format!("{} signals ({})", evidence.len(), parts.join(", "))
}

#[async_trait]
impl RuleCurator for AiRuleCurator {
    async fn curate(&self, request: CuratorRequest) -> Result<CuratorReport, CuratorError> {
        let kinds = Self::kinds(&request);
        let mut report = CuratorReport::default();

        // Which capabilities require the model (the others — conflict detection — are
        // deterministic and need no provider call).
        let ai_ops: Vec<String> = CuratorOperation::all()
            .into_iter()
            .filter(|op| {
                request.wants(*op)
                    && (op.is_proposing()
                        || matches!(
                            op,
                            CuratorOperation::SuggestThresholds
                                | CuratorOperation::SummarizeFeedback
                        ))
            })
            .map(|op| op.as_str().to_owned())
            .collect();

        if !ai_ops.is_empty() {
            let evidence = self.learning.collect_evidence(EvidenceQuery::all()).await?;
            let open_conflicts = self.conflicts.list_open().await?;
            let conflicts_summary = if open_conflicts.is_empty() {
                "(none)".to_owned()
            } else {
                open_conflicts
                    .iter()
                    .map(|c| format!("- {}: {}", c.id, c.description))
                    .collect::<Vec<_>>()
                    .join("\n")
            };

            let input = CurateRulesInput {
                operations: ai_ops,
                rules_summary: self.rules_summary(&kinds).await?,
                feedback_summary: feedback_summary_text(&evidence),
                conflicts_summary,
            };

            // A provider failure or invalid response is audited and drives no proposal.
            let response = match curate_rules(self.provider.as_ref(), input).await {
                Ok(response) => response,
                Err(error) => {
                    let entry = AuditEntry::new(event_type::PROVIDER_RESPONSE_REJECTED, Actor::Ai)
                        .with_payload(json!({
                            "task": "curate_rules",
                            "error": error.to_string(),
                        }));
                    self.audit.append(entry).await?;
                    return Err(error.into());
                }
            };

            for dto in response.proposals {
                if request.wants(operation_for(dto.proposal_type)) {
                    report.proposals.push(self.persist_proposal(dto).await?);
                }
            }

            if request.wants(CuratorOperation::SuggestThresholds) {
                report.threshold_suggestions.extend(
                    response
                        .threshold_suggestions
                        .into_iter()
                        .map(|t| ThresholdSuggestion {
                            rule_kind: t.rule_kind,
                            source_kind: t.source_kind,
                            suggested: t.suggested,
                            rationale: t.rationale,
                        }),
                );
            }

            if request.wants(CuratorOperation::SummarizeFeedback) {
                if let Some(text) = response.feedback_summary {
                    report.feedback_summaries.push(FeedbackSummary {
                        source_kind: dominant_source(&evidence),
                        sample_size: evidence.len(),
                        text,
                    });
                }
            }
        }

        if request.wants(CuratorOperation::DetectConflicts) {
            report.conflicts = self.scan_conflicts(&kinds).await?;
        }

        Ok(report)
    }
}
