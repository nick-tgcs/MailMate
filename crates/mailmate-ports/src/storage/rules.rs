//! The rule repository port: persist rule drafts and immutable versions, drive the
//! lifecycle status, and read the active/shadow snapshots the engines evaluate.
//!
//! The architecture sketches this as `RuleRepository<R: RuleKind>` over two fully separate
//! tables. In this codebase the Phase-4 rule domain already unified the two pipelines into
//! one [`EvaluatableRule`] carrying a [`RuleKind`] discriminant (one condition/effect AST,
//! one lifecycle state machine), so the generic collapses to a `kind`-discriminated trait:
//! every method that selects a table takes the [`RuleKind`], and the adapter routes to the
//! `classification_*` or `action_*` tables accordingly. One mechanism, two tables — exactly
//! the documented intent, realized over the unified domain.

use async_trait::async_trait;

use mailmate_common::error::StorageError;
use mailmate_common::ids::{RuleId, RuleVersionId};
use mailmate_common::rules::rule::{
    EvaluatableRule, NewRule, NewRuleVersion, RuleKind, RuleScope, RuleStatus,
};

/// Persistence for rules and their immutable versions.
#[async_trait]
pub trait RuleRepository: Send + Sync {
    /// The active-status rules of `kind` whose scope is `scope` — the snapshot the engine
    /// evaluates and applies.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn get_active_rules(
        &self,
        kind: RuleKind,
        scope: RuleScope,
    ) -> Result<Vec<EvaluatableRule>, StorageError>;

    /// The shadow-mode rules of `kind` whose scope is `scope` — evaluated and logged but
    /// never applied (the back-test/promotion substrate).
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn get_shadow_rules(
        &self,
        kind: RuleKind,
        scope: RuleScope,
    ) -> Result<Vec<EvaluatableRule>, StorageError>;

    /// Persist a new rule in [`Draft`](RuleStatus::Draft) status together with its first
    /// immutable version, atomically. Returns the new rule id.
    ///
    /// # Errors
    /// [`StorageError::Constraint`] on a duplicate `stable_name`, or other
    /// [`StorageError`] on a backend failure.
    async fn save_rule_draft(&self, draft: NewRule) -> Result<RuleId, StorageError>;

    /// Append a new immutable version to an existing rule, assigning the next monotonic
    /// version number and re-pointing the rule's current version. Returns the version id.
    ///
    /// # Errors
    /// [`StorageError::Constraint`] if the rule does not exist, or other [`StorageError`]
    /// on a backend failure.
    async fn create_rule_version(
        &self,
        version: NewRuleVersion,
    ) -> Result<RuleVersionId, StorageError>;

    /// Transition a rule to a new lifecycle status.
    ///
    /// # Errors
    /// [`StorageError`] on a backend failure.
    async fn update_rule_status(
        &self,
        rule_id: &RuleId,
        kind: RuleKind,
        status: RuleStatus,
    ) -> Result<(), StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn RuleRepository) {}
        let _ = takes as fn(&dyn RuleRepository);
    }
}
