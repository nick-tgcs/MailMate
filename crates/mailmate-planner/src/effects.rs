//! Translating a matched rule's [`RuleEffect`] into untrusted
//! [`ProposedAction`](mailmate_common::action::ProposedAction)s.
//!
//! This is the one place P2 turns a rule's declared effect into a candidate action. By
//! construction it only ever emits the five *safe* candidate kinds (tag / move / mark-junk /
//! require-review) — it cannot express a prohibited act (delete, send, open-link, …). That is
//! a structural floor beneath the policy guard: even before the guard runs, a rule can never
//! produce an unsafe candidate. Classification effects (`set_labels`, `priority`) belong to
//! P1 and are ignored here.

use mailmate_common::action::ProposedAction;
use mailmate_common::ids::{FolderId, MessageId};
use mailmate_common::rules::effect::RuleEffect;

/// Append the candidate actions implied by `effect` (for `message_id`) onto `out`.
pub fn translate_effect(
    effect: &RuleEffect,
    message_id: &MessageId,
    out: &mut Vec<ProposedAction>,
) {
    for tag in &effect.tag {
        out.push(ProposedAction::Tag {
            message_id: message_id.clone(),
            tag: tag.clone(),
        });
    }
    if let Some(folder) = &effect.move_to {
        out.push(ProposedAction::Move {
            message_id: message_id.clone(),
            to_folder: FolderId::from(folder.as_str()),
        });
    }
    if let Some(junk) = effect.mark_junk {
        out.push(ProposedAction::MarkJunk {
            message_id: message_id.clone(),
            junk,
        });
    }
    for kind in &effect.require_review_for {
        out.push(ProposedAction::RequireReview {
            target: format!("{kind} on message {message_id}"),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg() -> MessageId {
        MessageId::from("msg_1")
    }

    #[test]
    fn an_action_effect_translates_to_tag_move_junk_and_review() {
        let effect = RuleEffect {
            tag: vec!["receipt".to_owned()],
            move_to: Some("Receipts/Software".to_owned()),
            mark_junk: Some(true),
            require_review_for: vec!["move".to_owned()],
            ..RuleEffect::new()
        };
        let mut out = Vec::new();
        translate_effect(&effect, &msg(), &mut out);
        assert_eq!(out.len(), 4);
        assert!(matches!(out[0], ProposedAction::Tag { .. }));
        assert!(matches!(
            &out[1],
            ProposedAction::Move { to_folder, .. } if to_folder.as_str() == "Receipts/Software"
        ));
        assert!(matches!(
            out[2],
            ProposedAction::MarkJunk { junk: true, .. }
        ));
        assert!(matches!(out[3], ProposedAction::RequireReview { .. }));
    }

    #[test]
    fn classification_only_effects_produce_no_actions() {
        let effect = RuleEffect {
            set_labels: vec!["invoice".to_owned()],
            priority: Some("high".to_owned()),
            ..RuleEffect::new()
        };
        let mut out = Vec::new();
        translate_effect(&effect, &msg(), &mut out);
        assert!(out.is_empty(), "P1 effects are not P2 actions");
    }

    #[test]
    fn translation_can_never_emit_a_prohibited_action() {
        // The structural floor: whatever fields an effect carries, every produced candidate
        // projects onto a safe PlannedAction (none is delete/send/open-link/…).
        let effects = [
            RuleEffect {
                tag: vec!["a".to_owned(), "b".to_owned()],
                move_to: Some("X".to_owned()),
                mark_junk: Some(false),
                require_review_for: vec!["mark_junk".to_owned()],
                ..RuleEffect::new()
            },
            RuleEffect::new(),
        ];
        for effect in &effects {
            let mut out = Vec::new();
            translate_effect(effect, &msg(), &mut out);
            assert!(
                out.iter().all(|a| a.to_planned().is_some()),
                "every translated candidate must be safely applicable"
            );
        }
    }
}
