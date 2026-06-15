//! Behavioural tests for [`ImportExportService`]: export sweeps the operative rule set,
//! import lands every rule as a non-firing draft, name collisions are skipped, and an
//! unknown manifest version is refused. Driven over a local in-memory `RuleRepository` fake
//! (the service names only the port).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::executor::block_on;

use mailmate_common::error::StorageError;
use mailmate_common::ids::{RuleId, RuleVersionId};
use mailmate_common::rules::condition::Condition;
use mailmate_common::rules::effect::RuleEffect;
use mailmate_common::rules::manifest::{ExportedRule, RuleManifest};
use mailmate_common::rules::rule::{
    EvaluatableRule, HierarchyBand, NewRule, NewRuleVersion, RiskLevel, RuleKind, RuleScope,
    RuleStatus, RuleVersion,
};
use mailmate_core::ImportExportService;
use mailmate_ports::storage::rules::RuleRepository;

/// A minimal in-memory `RuleRepository`: programmed active/shadow snapshots, and it records
/// the drafts an import creates (rejecting a duplicate `stable_name` like the real store).
#[derive(Default)]
struct FakeRules {
    active: Vec<EvaluatableRule>,
    shadow: Vec<EvaluatableRule>,
    saved: Mutex<Vec<NewRule>>,
}

#[async_trait]
impl RuleRepository for FakeRules {
    async fn get_active_rules(
        &self,
        kind: RuleKind,
        scope: RuleScope,
    ) -> Result<Vec<EvaluatableRule>, StorageError> {
        Ok(self
            .active
            .iter()
            .filter(|r| r.kind == kind && r.scope == scope)
            .cloned()
            .collect())
    }

    async fn get_shadow_rules(
        &self,
        kind: RuleKind,
        scope: RuleScope,
    ) -> Result<Vec<EvaluatableRule>, StorageError> {
        Ok(self
            .shadow
            .iter()
            .filter(|r| r.kind == kind && r.scope == scope)
            .cloned()
            .collect())
    }

    async fn save_rule_draft(&self, draft: NewRule) -> Result<RuleId, StorageError> {
        let mut saved = self.saved.lock().unwrap();
        if saved.iter().any(|r| r.stable_name == draft.stable_name) {
            return Err(StorageError::Constraint(format!(
                "duplicate stable_name {}",
                draft.stable_name
            )));
        }
        let id = RuleId::from(format!("rule_{}", draft.stable_name));
        saved.push(draft);
        Ok(id)
    }

    async fn create_rule_version(
        &self,
        _version: NewRuleVersion,
    ) -> Result<RuleVersionId, StorageError> {
        Ok(RuleVersionId::fresh())
    }

    async fn update_rule_status(
        &self,
        _rule_id: &RuleId,
        _kind: RuleKind,
        _status: RuleStatus,
    ) -> Result<(), StorageError> {
        Ok(())
    }
}

fn rule(id: &str, kind: RuleKind, scope: RuleScope, status: RuleStatus) -> EvaluatableRule {
    EvaluatableRule {
        rule_id: RuleId::from(id),
        kind,
        scope,
        band: HierarchyBand::LearnedActive,
        status,
        version: RuleVersion {
            id: RuleVersionId::from("rv_1"),
            version_number: 1,
            condition: Condition::All { all: vec![] },
            effect: RuleEffect {
                set_labels: vec!["receipt".to_owned()],
                ..RuleEffect::default()
            },
            risk_level: RiskLevel::Low,
        },
    }
}

fn exported(id: &str, kind: RuleKind) -> ExportedRule {
    ExportedRule {
        source_rule_id: id.to_owned(),
        kind,
        scope: RuleScope::Global,
        band: HierarchyBand::LearnedActive,
        status: RuleStatus::Active,
        condition: Condition::All { all: vec![] },
        effect: RuleEffect::default(),
        risk_level: RiskLevel::Low,
    }
}

#[test]
fn export_sweeps_active_and_shadow_across_kinds_and_scopes() {
    let repo = FakeRules {
        active: vec![rule(
            "rule_a",
            RuleKind::Classification,
            RuleScope::Global,
            RuleStatus::Active,
        )],
        shadow: vec![rule(
            "rule_b",
            RuleKind::Action,
            RuleScope::Domain,
            RuleStatus::ShadowMode,
        )],
        saved: Mutex::new(vec![]),
    };
    let manifest = block_on(ImportExportService::new(Arc::new(repo)).export()).unwrap();
    assert_eq!(manifest.len(), 2);
    let ids: Vec<&str> = manifest
        .rules
        .iter()
        .map(|r| r.source_rule_id.as_str())
        .collect();
    assert!(ids.contains(&"rule_a") && ids.contains(&"rule_b"));
    let a = manifest
        .rules
        .iter()
        .find(|r| r.source_rule_id == "rule_a")
        .unwrap();
    assert_eq!(a.effect.set_labels, vec!["receipt".to_owned()]);
}

#[test]
fn import_creates_drafts_and_skips_duplicate_names() {
    let service = ImportExportService::new(Arc::new(FakeRules::default()));
    // Two rules sharing a source id collide on the derived `<prefix>-<id>` name.
    let manifest = RuleManifest::new(vec![
        exported("r1", RuleKind::Classification),
        exported("r1", RuleKind::Action),
    ]);
    let summary = block_on(service.import(&manifest, "imported")).unwrap();
    assert_eq!(summary.imported, 1);
    assert_eq!(summary.skipped.len(), 1);
    assert_eq!(summary.skipped[0].source_rule_id, "r1");
}

#[test]
fn import_refuses_an_unknown_manifest_version() {
    let service = ImportExportService::new(Arc::new(FakeRules::default()));
    let manifest = RuleManifest {
        manifest_version: 999,
        rules: vec![],
    };
    let err = block_on(service.import(&manifest, "imported")).unwrap_err();
    assert!(matches!(err, StorageError::Serialization(_)), "got {err:?}");
}

#[test]
fn export_then_import_round_trips_the_operative_rule_as_a_draft() {
    let repo = Arc::new(FakeRules {
        active: vec![rule(
            "rule_keep",
            RuleKind::Classification,
            RuleScope::Global,
            RuleStatus::Active,
        )],
        ..FakeRules::default()
    });
    let service = ImportExportService::new(repo.clone());
    let manifest = block_on(service.export()).unwrap();
    let summary = block_on(service.import(&manifest, "copy")).unwrap();
    assert_eq!(summary.imported, 1);
    let saved = repo.saved.lock().unwrap();
    assert_eq!(saved[0].stable_name, "copy-rule_keep");
    // The operative effect round-trips; the import lands as a User-authored draft.
    assert_eq!(
        saved[0].initial_version.effect.set_labels,
        vec!["receipt".to_owned()]
    );
    assert_eq!(saved[0].created_by, mailmate_common::actor::Actor::User);
}
