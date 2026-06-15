//! The rule import/export use-case: serialize the operative rule set to a portable
//! [`RuleManifest`], and re-create rules from one as fresh drafts.
//!
//! It names only the [`RuleRepository`] port. **Export** reads the rules that actually decide
//! behaviour — the `active` and `shadow` rules of both kinds, across every scope — and strips
//! their engine-internal identity into identity-free [`ExportedRule`]s. **Import** is
//! deliberately conservative: every rule lands as a [`Draft`](mailmate_common::rules::rule::RuleStatus::Draft)
//! (the repository forces it), so an imported manifest can never silently activate a rule —
//! a human still has to review and activate each one. A name collision is skipped, not
//! overwritten, so importing onto a populated store never clobbers an existing rule.

use std::sync::Arc;

use mailmate_common::actor::Actor;
use mailmate_common::error::StorageError;
use mailmate_common::rules::manifest::{ExportedRule, RuleManifest, MANIFEST_VERSION};
use mailmate_common::rules::rule::{
    EvaluatableRule, NewRule, RuleKind, RuleScope, RuleVersionContent,
};
use mailmate_ports::storage::rules::RuleRepository;

/// Every rule kind export sweeps.
const KINDS: [RuleKind; 2] = [RuleKind::Classification, RuleKind::Action];

/// Every scope export sweeps.
const SCOPES: [RuleScope; 5] = [
    RuleScope::Global,
    RuleScope::Account,
    RuleScope::Folder,
    RuleScope::Sender,
    RuleScope::Domain,
];

/// The outcome of an import: how many rules were created, and which were skipped (and why).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    /// The rules created as drafts.
    pub imported: usize,
    /// The rules skipped (e.g. a name collision), with a reason each.
    pub skipped: Vec<SkippedRule>,
}

/// One rule an import skipped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedRule {
    /// The manifest's source-rule id (the import-name basis).
    pub source_rule_id: String,
    /// Why it was skipped.
    pub reason: String,
}

/// Imports and exports rules through the rule repository.
#[derive(Clone)]
pub struct ImportExportService {
    rules: Arc<dyn RuleRepository>,
}

impl ImportExportService {
    /// Assemble the service over the rule repository port.
    #[must_use]
    pub fn new(rules: Arc<dyn RuleRepository>) -> Self {
        Self { rules }
    }

    /// Export the operative rule set (active + shadow, both kinds, every scope) as a
    /// [`RuleManifest`].
    ///
    /// # Errors
    /// Propagates a [`StorageError`] from the repository.
    pub async fn export(&self) -> Result<RuleManifest, StorageError> {
        let mut exported = Vec::new();
        for kind in KINDS {
            for scope in SCOPES {
                for rule in self.rules.get_active_rules(kind, scope).await? {
                    exported.push(to_exported(&rule));
                }
                for rule in self.rules.get_shadow_rules(kind, scope).await? {
                    exported.push(to_exported(&rule));
                }
            }
        }
        Ok(RuleManifest::new(exported))
    }

    /// Import every rule in `manifest` as a fresh draft, naming each `<prefix>-<source-id>`.
    /// A rule whose name already exists is skipped (never overwritten). An unknown manifest
    /// version is refused.
    ///
    /// # Errors
    /// [`StorageError::Serialization`] if `manifest_version` is unsupported, or a
    /// non-constraint [`StorageError`] from the repository (a constraint violation is
    /// recorded as a skip, not an error).
    pub async fn import(
        &self,
        manifest: &RuleManifest,
        name_prefix: &str,
    ) -> Result<ImportSummary, StorageError> {
        if manifest.manifest_version != MANIFEST_VERSION {
            return Err(StorageError::Serialization(format!(
                "unsupported rule-manifest version {} (this build understands {MANIFEST_VERSION})",
                manifest.manifest_version
            )));
        }
        let mut summary = ImportSummary::default();
        for rule in &manifest.rules {
            let new_rule = to_new_rule(rule, name_prefix);
            match self.rules.save_rule_draft(new_rule).await {
                Ok(_) => summary.imported += 1,
                // A duplicate name is a skip, not a failure — re-importing is idempotent-ish.
                Err(StorageError::Constraint(_)) => summary.skipped.push(SkippedRule {
                    source_rule_id: rule.source_rule_id.clone(),
                    reason: "a rule with this name already exists".to_owned(),
                }),
                Err(other) => return Err(other),
            }
        }
        Ok(summary)
    }
}

/// Strip an engine-facing rule into its identity-free manifest form.
fn to_exported(rule: &EvaluatableRule) -> ExportedRule {
    ExportedRule {
        source_rule_id: rule.rule_id.as_str().to_owned(),
        kind: rule.kind,
        scope: rule.scope,
        band: rule.band,
        status: rule.status,
        condition: rule.version.condition.clone(),
        effect: rule.version.effect.clone(),
        risk_level: rule.version.risk_level,
    }
}

/// Re-create a manifest rule as a new draft. Cosmetic version metadata is regenerated; the
/// condition/effect/risk/band/scope/kind round-trip.
fn to_new_rule(rule: &ExportedRule, name_prefix: &str) -> NewRule {
    NewRule {
        stable_name: format!("{name_prefix}-{}", rule.source_rule_id),
        kind: rule.kind,
        scope: rule.scope,
        band: rule.band,
        created_by: Actor::User,
        initial_version: RuleVersionContent {
            title: format!("Imported rule {}", rule.source_rule_id),
            description: "Imported from a rule manifest; review before activating.".to_owned(),
            condition: rule.condition.clone(),
            effect: rule.effect.clone(),
            priority: 0,
            confidence_threshold: None,
            risk_level: rule.risk_level,
            change_reason: "imported from manifest".to_owned(),
            created_by: Actor::User,
        },
    }
}
