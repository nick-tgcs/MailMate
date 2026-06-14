//! Rule effects. Classification effects (set labels, priority) and action effects (tag,
//! move, junk, require-review) share one struct with optional fields, matching the
//! `effect_json` examples in the spec; which fields are populated depends on the rule kind.

use serde::{Deserialize, Serialize};

/// The structured effect a matched rule contributes. All fields optional; an action rule
/// populates the action fields, a classification rule the classification fields.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuleEffect {
    /// Tags to apply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tag: Vec<String>,
    /// Destination folder for a move.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "move")]
    pub move_to: Option<String>,
    /// Junk state to set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark_junk: Option<bool>,
    /// Action kinds that must be held for human review (e.g. `["move", "mark_junk"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require_review_for: Vec<String>,
    /// Classification labels to set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub set_labels: Vec<String>,
    /// Priority to set (e.g. `"normal"`, `"high"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
}

impl RuleEffect {
    /// An empty effect (no-op).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether two effects directly contradict each other — the core of
    /// `contradictory_effect` conflict detection: a move to two different folders, or
    /// opposite junk states.
    #[must_use]
    pub fn contradicts(&self, other: &Self) -> bool {
        let move_conflict = matches!(
            (&self.move_to, &other.move_to),
            (Some(a), Some(b)) if a != b
        );
        let junk_conflict = matches!(
            (self.mark_junk, other.mark_junk),
            (Some(a), Some(b)) if a != b
        );
        move_conflict || junk_conflict
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn move_field_serializes_as_move() {
        let effect = RuleEffect {
            move_to: Some("Projects/Updates".to_owned()),
            ..RuleEffect::new()
        };
        let value = serde_json::to_value(&effect).unwrap();
        assert_eq!(value, json!({ "move": "Projects/Updates" }));
        let back: RuleEffect = serde_json::from_value(value).unwrap();
        assert_eq!(back, effect);
    }

    #[test]
    fn tag_and_priority_effect_round_trips() {
        let value = json!({ "tag": ["receipt"], "priority": "normal" });
        let effect: RuleEffect = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(effect.tag, vec!["receipt".to_owned()]);
        assert_eq!(effect.priority.as_deref(), Some("normal"));
        assert_eq!(serde_json::to_value(&effect).unwrap(), value);
    }

    #[test]
    fn contradiction_detects_divergent_moves_and_junk() {
        let to_a = RuleEffect {
            move_to: Some("A".to_owned()),
            ..RuleEffect::new()
        };
        let to_b = RuleEffect {
            move_to: Some("B".to_owned()),
            ..RuleEffect::new()
        };
        assert!(to_a.contradicts(&to_b));
        assert!(!to_a.contradicts(&to_a));

        let junk = RuleEffect {
            mark_junk: Some(true),
            ..RuleEffect::new()
        };
        let not_junk = RuleEffect {
            mark_junk: Some(false),
            ..RuleEffect::new()
        };
        assert!(junk.contradicts(&not_junk));

        // A tag and a move do not contradict — they compose.
        let tag = RuleEffect {
            tag: vec!["x".to_owned()],
            ..RuleEffect::new()
        };
        assert!(!tag.contradicts(&to_a));
    }
}
