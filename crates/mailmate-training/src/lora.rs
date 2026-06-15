//! Adapter registration: importing an externally-trained adapter from its metadata, and
//! turning a registered adapter into a spec a provider can run a compatibility check
//! against.
//!
//! An imported adapter is always registered as a **candidate** — importing metadata never
//! activates an adapter. Promotion to `active` is a separate, gated decision (see
//! [`crate::evaluation`]). The compatibility check itself lives in `mailmate-common` (so the
//! provider port and this crate share one implementation); it is re-exported here for
//! convenience.

pub use mailmate_common::adapter::{check_compatibility, AdapterCompatibility, BaseModelTarget};

use mailmate_common::adapter::{AdapterMetadata, AdapterSpec};
use mailmate_common::time::Timestamp;
use mailmate_common::training::{AdapterStatus, LoraAdapterRecord};

/// Register an externally-trained adapter from its metadata as a **candidate**, recording
/// where its artifact lives. The status is always [`AdapterStatus::Candidate`]; an import is
/// never an activation.
#[must_use]
pub fn import_adapter(
    metadata: &AdapterMetadata,
    artifact_path: impl Into<String>,
) -> LoraAdapterRecord {
    LoraAdapterRecord {
        id: metadata.adapter_id.clone(),
        name: metadata.name.clone(),
        adapter_type: metadata.adapter_type,
        format: metadata.format,
        base_model_family: metadata.base_model_family.clone(),
        base_model_name: metadata.base_model_name.clone(),
        base_model_revision: metadata.base_model_revision.clone(),
        tokenizer_hash: metadata.tokenizer_hash.clone(),
        chat_template_hash: metadata.chat_template_hash.clone(),
        training_dataset_id: metadata.training_dataset_id.clone(),
        artifact_path: artifact_path.into(),
        status: AdapterStatus::Candidate,
        created_at: metadata.created_at,
    }
}

/// Register a locally-trained artifact as a **candidate** adapter record.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn register_local_adapter(
    id: mailmate_common::ids::AdapterId,
    name: impl Into<String>,
    artifact: &crate::trainer::TrainedArtifact,
    training_dataset_id: mailmate_common::ids::DatasetId,
    now: Timestamp,
) -> LoraAdapterRecord {
    LoraAdapterRecord {
        id,
        name: name.into(),
        adapter_type: artifact.adapter_type,
        format: artifact.format,
        base_model_family: artifact.base_model_family.clone(),
        base_model_name: artifact.base_model_name.clone(),
        base_model_revision: None,
        tokenizer_hash: artifact.tokenizer_hash.clone(),
        chat_template_hash: artifact.chat_template_hash.clone(),
        training_dataset_id: Some(training_dataset_id),
        artifact_path: artifact.artifact_path.clone(),
        status: AdapterStatus::Candidate,
        created_at: now,
    }
}

/// The [`AdapterSpec`] a provider runs a compatibility check / load against for `record`.
#[must_use]
pub fn spec_for(record: &LoraAdapterRecord) -> AdapterSpec {
    AdapterSpec {
        adapter_id: record.id.clone(),
        path: record.artifact_path.clone(),
        adapter_type: record.adapter_type,
        base_model_family: record.base_model_family.clone(),
        tokenizer_hash: record.tokenizer_hash.clone(),
        chat_template_hash: record.chat_template_hash.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailmate_common::ids::{AdapterId, DatasetId};
    use mailmate_common::training::{AdapterFormat, AdapterType};

    fn metadata() -> AdapterMetadata {
        AdapterMetadata {
            adapter_id: AdapterId::from("lora_001"),
            name: "mailmate-draft-style-v1".to_owned(),
            format: AdapterFormat::Safetensors,
            adapter_type: AdapterType::Lora,
            base_model_family: "llama".to_owned(),
            base_model_name: "Meta-Llama-3.1-8B-Instruct".to_owned(),
            base_model_revision: Some("rev_abc".to_owned()),
            tokenizer_hash: Some("tok_abc".to_owned()),
            chat_template_hash: Some("tmpl_def".to_owned()),
            training_dataset_id: None,
            training_example_count: 1200,
            positive_count: 850,
            negative_count: 350,
            created_at: Timestamp::now(),
            eval_report_id: None,
        }
    }

    #[test]
    fn import_registers_a_candidate_never_active() {
        let record = import_adapter(&metadata(), "/adapters/lora_001.safetensors");
        assert_eq!(record.status, AdapterStatus::Candidate);
        assert!(!record.status.is_active(), "import must never activate");
        assert_eq!(record.id, AdapterId::from("lora_001"));
        assert_eq!(record.base_model_family, "llama");
        assert_eq!(record.tokenizer_hash.as_deref(), Some("tok_abc"));
        assert!(
            record.training_dataset_id.is_none(),
            "external import has no local dataset"
        );
        assert_eq!(record.artifact_path, "/adapters/lora_001.safetensors");
    }

    #[test]
    fn spec_for_round_trips_into_a_compatibility_check() {
        let record = import_adapter(&metadata(), "/p");
        let spec = spec_for(&record);
        let target = BaseModelTarget {
            family: "llama".to_owned(),
            tokenizer_hash: Some("tok_abc".to_owned()),
            chat_template_hash: Some("tmpl_def".to_owned()),
        };
        assert!(check_compatibility(&spec, &target).is_compatible());
        // A different family is rejected.
        let other = BaseModelTarget {
            family: "qwen".to_owned(),
            ..target
        };
        assert!(check_compatibility(&spec, &other).is_incompatible());
    }

    #[test]
    fn local_registration_marks_candidate_and_links_the_dataset() {
        use crate::trainer::{TrainedArtifact, TrainedArtifactKind};
        use std::collections::BTreeMap;
        let artifact = TrainedArtifact {
            kind: TrainedArtifactKind::LoraAdapter,
            artifact_path: "/adapters/local.safetensors".to_owned(),
            format: AdapterFormat::Safetensors,
            adapter_type: AdapterType::Lora,
            base_model_family: "llama".to_owned(),
            base_model_name: "llama-3".to_owned(),
            tokenizer_hash: Some("tok_x".to_owned()),
            chat_template_hash: None,
            example_count: 10,
            training_metrics: BTreeMap::new(),
        };
        let record = register_local_adapter(
            AdapterId::from("lora_local"),
            "nightly",
            &artifact,
            DatasetId::from("ds_1"),
            Timestamp::now(),
        );
        assert_eq!(record.status, AdapterStatus::Candidate);
        assert_eq!(record.training_dataset_id, Some(DatasetId::from("ds_1")));
        assert_eq!(record.base_model_name, "llama-3");
    }
}
