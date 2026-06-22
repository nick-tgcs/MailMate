//! The category vocabulary: the human-readable labels the per-message panel offers for a
//! one-click category correction, and that classification labels render through.
//!
//! Phase 1b ships a deterministic **starter** vocabulary so the panel has human names instead
//! of raw labels from session one. It is surfaced on `get_settings` (the extension never
//! hardcodes it). Later phases make it user-editable and join the user's tag keys to it (the
//! tag→category mapping surface) — this module is the single source of the default set.

use serde::{Deserialize, Serialize};

/// One entry in the category vocabulary: a stable `key` (used in labels/corrections) and the
/// human `label` the panel shows.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CategoryDescriptor {
    /// The stable category key (snake/kebab-free, lowercase) — what a correction records.
    pub key: String,
    /// The human-readable label shown in the panel.
    pub label: String,
}

impl CategoryDescriptor {
    fn new(key: &str, label: &str) -> Self {
        Self {
            key: key.to_owned(),
            label: label.to_owned(),
        }
    }
}

/// The deterministic starter category vocabulary, in display order. A small, common set —
/// enough to make a one-click "this is actually a …" correction useful at cold-start.
#[must_use]
pub fn starter_categories() -> Vec<CategoryDescriptor> {
    [
        ("personal", "Personal"),
        ("work", "Work"),
        ("newsletters", "Newsletters"),
        ("promotions", "Promotions"),
        ("receipts", "Receipts"),
        ("finance", "Finance"),
        ("social", "Social"),
        ("travel", "Travel"),
        ("updates", "Updates"),
        ("spam", "Spam"),
    ]
    .into_iter()
    .map(|(k, l)| CategoryDescriptor::new(k, l))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_starter_vocabulary_is_non_empty_with_unique_keys_and_human_labels() {
        let cats = starter_categories();
        assert!(cats.len() >= 5);
        // Keys are unique and lowercase; labels are human (start uppercase).
        let mut keys: Vec<&str> = cats.iter().map(|c| c.key.as_str()).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before, "category keys are unique");
        for c in &cats {
            assert_eq!(c.key, c.key.to_ascii_lowercase(), "keys are lowercase");
            assert!(
                c.label.chars().next().is_some_and(char::is_uppercase),
                "label is human: {:?}",
                c.label
            );
        }
        // A couple of the obvious ones are present.
        assert!(cats.iter().any(|c| c.key == "newsletters"));
        assert!(cats.iter().any(|c| c.key == "receipts"));
    }
}
