//! The portable rule **manifest**: the serialization format for rule import/export.
//!
//! A manifest captures the *operative* essence of a rule — its kind, scope, authority band,
//! lifecycle status, and its current condition→effect (with risk) — in a backend-neutral,
//! human-diffable JSON shape. It deliberately does NOT carry the engine-internal ids
//! (`rule_…`/`rv_…`) or the cosmetic version metadata (monotonic number, change-reason,
//! author): an import re-creates each rule as a fresh **draft** (never active), so those are
//! regenerated rather than transplanted. What round-trips is the part that decides behaviour
//! — the condition, the effect, the band, and the scope.
//!
//! This is a pure value type. The read/write-the-store orchestration is a core use-case
//! (`mailmate_core::ImportExportService`); the file I/O is at the edge (the host).

use serde::{Deserialize, Serialize};

use crate::rules::condition::Condition;
use crate::rules::effect::RuleEffect;
use crate::rules::rule::{HierarchyBand, RiskLevel, RuleKind, RuleScope, RuleStatus};

/// The manifest format version. Bumped only on a breaking shape change so an importer can
/// refuse a manifest it does not understand.
pub const MANIFEST_VERSION: u32 = 1;

/// A portable bundle of exported rules.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RuleManifest {
    /// The format version (see [`MANIFEST_VERSION`]).
    pub manifest_version: u32,
    /// The exported rules, in a stable order.
    pub rules: Vec<ExportedRule>,
}

impl RuleManifest {
    /// A manifest stamped with the current [`MANIFEST_VERSION`].
    #[must_use]
    pub fn new(rules: Vec<ExportedRule>) -> Self {
        Self {
            manifest_version: MANIFEST_VERSION,
            rules,
        }
    }

    /// How many rules the manifest carries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether the manifest carries no rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// One rule's operative content, identity-free.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExportedRule {
    /// The source rule's id, kept as the basis for a deterministic import name and as a
    /// provenance breadcrumb — never used to overwrite an existing rule on import.
    pub source_rule_id: String,
    /// Which pipeline it belongs to.
    pub kind: RuleKind,
    /// Its scope.
    pub scope: RuleScope,
    /// Its authority band (preserved for round-trip fidelity; an imported rule is still a
    /// non-firing draft until a human activates it).
    pub band: HierarchyBand,
    /// The source rule's lifecycle status at export time — informational only (an import
    /// always lands as a draft).
    pub status: RuleStatus,
    /// The current version's condition tree.
    pub condition: Condition,
    /// The current version's effect.
    pub effect: RuleEffect,
    /// The current version's risk level.
    pub risk_level: RiskLevel,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::condition::Condition;
    use crate::rules::effect::RuleEffect;

    fn exported() -> ExportedRule {
        ExportedRule {
            source_rule_id: "rule_abc".to_owned(),
            kind: RuleKind::Classification,
            scope: RuleScope::Global,
            band: HierarchyBand::LearnedActive,
            status: RuleStatus::Active,
            condition: Condition::All { all: vec![] },
            effect: RuleEffect::default(),
            risk_level: RiskLevel::Low,
        }
    }

    #[test]
    fn manifest_stamps_the_current_version_and_round_trips_json() {
        let manifest = RuleManifest::new(vec![exported()]);
        assert_eq!(manifest.manifest_version, MANIFEST_VERSION);
        assert_eq!(manifest.len(), 1);
        assert!(!manifest.is_empty());

        let json = serde_json::to_string(&manifest).unwrap();
        let back: RuleManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, manifest, "the manifest round-trips losslessly");
    }

    #[test]
    fn an_empty_manifest_is_empty() {
        let manifest = RuleManifest::new(vec![]);
        assert!(manifest.is_empty());
        assert_eq!(manifest.len(), 0);
    }
}
