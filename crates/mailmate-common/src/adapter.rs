//! Adapter loading is an **optional provider capability**, expressed as metadata rather
//! than assumed of any backend. These types describe an adapter to a provider and capture
//! the verdict of a compatibility check — they never assume a particular engine, and a
//! provider that cannot load adapters simply never implements the `SupportsAdapters` seam.
//!
//! A LoRA adapter is portable only across compatible base-model families, tokenizers, and
//! chat templates. [`AdapterMetadata`] carries that compatibility metadata with every
//! imported artifact; [`check_compatibility`] turns a spec + a target into a verdict so the
//! portability rule ("never portable across incompatible base models without compatibility
//! metadata") is enforced in code, not prose.

use serde::{Deserialize, Serialize};

use crate::ids::{AdapterId, DatasetId, EvalRunId};
use crate::time::Timestamp;
use crate::training::{AdapterFormat, AdapterType};

/// A request to load an adapter onto a provider. `path` is a plain string (not a
/// `PathBuf`): this is leaf-of-the-hexagon data that round-trips through JSON and a `TEXT`
/// column, and must not vary by platform.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdapterSpec {
    /// The adapter being loaded.
    pub adapter_id: AdapterId,
    /// Where the artifact lives on disk.
    pub path: String,
    /// The adapter kind.
    pub adapter_type: AdapterType,
    /// The base-model family the adapter was trained for.
    pub base_model_family: String,
    /// The tokenizer hash it requires, when known.
    pub tokenizer_hash: Option<String>,
    /// The chat-template hash it requires, when known.
    pub chat_template_hash: Option<String>,
}

/// The base model an adapter would be layered onto, for a compatibility check.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct BaseModelTarget {
    /// The target model's family (`llama`, `qwen`, …).
    pub family: String,
    /// The target tokenizer hash, when known.
    pub tokenizer_hash: Option<String>,
    /// The target chat-template hash, when known.
    pub chat_template_hash: Option<String>,
}

/// The verdict of a compatibility check between an adapter and a base model.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AdapterCompatibility {
    /// Families match and no known hash conflicts — safe to load.
    Compatible,
    /// A hard mismatch (e.g. different families): the adapter must not be loaded.
    Incompatible {
        /// One reason per failed check.
        reasons: Vec<String>,
    },
    /// The families match but a hash could not be confirmed (one side is unknown): loadable
    /// only behind an explicit override, never silently.
    Unknown {
        /// What could not be confirmed.
        reasons: Vec<String>,
    },
}

impl AdapterCompatibility {
    /// Whether the adapter is confirmed safe to load with no caveats.
    #[must_use]
    pub fn is_compatible(&self) -> bool {
        matches!(self, Self::Compatible)
    }

    /// Whether loading is *forbidden* (a hard mismatch).
    #[must_use]
    pub fn is_incompatible(&self) -> bool {
        matches!(self, Self::Incompatible { .. })
    }
}

/// Check whether `adapter` may be loaded onto `target`.
///
/// A differing base-model family is a hard [`AdapterCompatibility::Incompatible`]. With
/// matching families, any *known-vs-known* hash mismatch (tokenizer or chat template) is
/// also incompatible; a hash that is unknown on either side downgrades the verdict to
/// [`AdapterCompatibility::Unknown`] rather than silently passing.
#[must_use]
pub fn check_compatibility(
    adapter: &AdapterSpec,
    target: &BaseModelTarget,
) -> AdapterCompatibility {
    if !adapter
        .base_model_family
        .eq_ignore_ascii_case(&target.family)
    {
        return AdapterCompatibility::Incompatible {
            reasons: vec![format!(
                "base model family mismatch: adapter `{}` vs target `{}`",
                adapter.base_model_family, target.family
            )],
        };
    }

    let mut hard = Vec::new();
    let mut unknown = Vec::new();
    compare_hash(
        "tokenizer",
        &adapter.tokenizer_hash,
        &target.tokenizer_hash,
        &mut hard,
        &mut unknown,
    );
    compare_hash(
        "chat template",
        &adapter.chat_template_hash,
        &target.chat_template_hash,
        &mut hard,
        &mut unknown,
    );

    if !hard.is_empty() {
        AdapterCompatibility::Incompatible { reasons: hard }
    } else if !unknown.is_empty() {
        AdapterCompatibility::Unknown { reasons: unknown }
    } else {
        AdapterCompatibility::Compatible
    }
}

fn compare_hash(
    what: &str,
    adapter: &Option<String>,
    target: &Option<String>,
    hard: &mut Vec<String>,
    unknown: &mut Vec<String>,
) {
    match (adapter, target) {
        (Some(a), Some(t)) if a != t => hard.push(format!("{what} hash mismatch: `{a}` vs `{t}`")),
        (Some(_), Some(_)) => {}
        _ => unknown.push(format!("{what} hash could not be confirmed")),
    }
}

/// Compatibility + provenance metadata for an externally-trained adapter, imported to
/// register the artifact. `deny_unknown_fields` so a malformed sidecar is rejected rather
/// than silently dropping fields.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterMetadata {
    /// The adapter id (`lora_…`).
    pub adapter_id: AdapterId,
    /// Adapter name.
    pub name: String,
    /// The weight format.
    pub format: AdapterFormat,
    /// The adapter kind.
    pub adapter_type: AdapterType,
    /// Compatible base-model family.
    pub base_model_family: String,
    /// The exact base model it was trained against.
    pub base_model_name: String,
    /// Base model revision / hash, when known.
    #[serde(default)]
    pub base_model_revision: Option<String>,
    /// Tokenizer compatibility hash, when known.
    #[serde(default)]
    pub tokenizer_hash: Option<String>,
    /// Chat-template compatibility hash, when known.
    #[serde(default)]
    pub chat_template_hash: Option<String>,
    /// The local dataset it was trained on, when there is one.
    #[serde(default)]
    pub training_dataset_id: Option<DatasetId>,
    /// How many examples it was trained on.
    pub training_example_count: usize,
    /// Positive example count.
    pub positive_count: usize,
    /// Negative example count.
    pub negative_count: usize,
    /// When it was created.
    pub created_at: Timestamp,
    /// The eval report that validated it, when one exists.
    #[serde(default)]
    pub eval_report_id: Option<EvalRunId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(family: &str, tok: Option<&str>, tmpl: Option<&str>) -> AdapterSpec {
        AdapterSpec {
            adapter_id: AdapterId::from("lora_1"),
            path: "/adapters/lora_1.safetensors".to_owned(),
            adapter_type: AdapterType::Lora,
            base_model_family: family.to_owned(),
            tokenizer_hash: tok.map(str::to_owned),
            chat_template_hash: tmpl.map(str::to_owned),
        }
    }

    fn target(family: &str, tok: Option<&str>, tmpl: Option<&str>) -> BaseModelTarget {
        BaseModelTarget {
            family: family.to_owned(),
            tokenizer_hash: tok.map(str::to_owned),
            chat_template_hash: tmpl.map(str::to_owned),
        }
    }

    #[test]
    fn matching_family_and_hashes_is_compatible() {
        let v = check_compatibility(
            &spec("llama", Some("tok_a"), Some("tmpl_a")),
            &target("Llama", Some("tok_a"), Some("tmpl_a")),
        );
        assert!(
            v.is_compatible(),
            "case-insensitive family + equal hashes -> compatible: {v:?}"
        );
        assert!(!v.is_incompatible());
    }

    #[test]
    fn differing_family_is_a_hard_incompatibility() {
        let v = check_compatibility(
            &spec("llama", Some("tok_a"), None),
            &target("qwen", Some("tok_a"), None),
        );
        assert!(v.is_incompatible());
        if let AdapterCompatibility::Incompatible { reasons } = v {
            assert!(reasons[0].contains("family mismatch"));
        }
    }

    #[test]
    fn known_hash_mismatch_is_incompatible() {
        let v = check_compatibility(
            &spec("llama", Some("tok_a"), None),
            &target("llama", Some("tok_b"), None),
        );
        assert!(v.is_incompatible(), "tokenizer mismatch must block: {v:?}");
    }

    #[test]
    fn unknown_hash_downgrades_to_unknown_not_compatible() {
        let v = check_compatibility(
            &spec("llama", None, Some("tmpl_a")),
            &target("llama", Some("tok_a"), Some("tmpl_a")),
        );
        assert!(
            !v.is_compatible(),
            "an unconfirmed hash must not silently pass"
        );
        assert!(matches!(v, AdapterCompatibility::Unknown { .. }));
    }

    #[test]
    fn metadata_rejects_unknown_fields() {
        let good = r#"{
            "adapter_id": "lora_1", "name": "draft-style-v1", "format": "safetensors",
            "adapter_type": "lora", "base_model_family": "llama",
            "base_model_name": "Meta-Llama-3.1-8B-Instruct",
            "training_example_count": 1200, "positive_count": 850, "negative_count": 350,
            "created_at": "2026-01-20T12:00:00Z"
        }"#;
        let meta: AdapterMetadata = serde_json::from_str(good).unwrap();
        assert_eq!(meta.positive_count, 850);
        assert!(meta.training_dataset_id.is_none());

        let bad = r#"{
            "adapter_id": "lora_1", "name": "x", "format": "safetensors",
            "adapter_type": "lora", "base_model_family": "llama", "base_model_name": "m",
            "training_example_count": 1, "positive_count": 1, "negative_count": 0,
            "created_at": "2026-01-20T12:00:00Z", "rogue_field": true
        }"#;
        assert!(
            serde_json::from_str::<AdapterMetadata>(bad).is_err(),
            "unknown field must be rejected"
        );
    }
}
