//! Application-generated, prefixed-string identifiers.
//!
//! Every id is an opaque `<prefix>_<uuid-simple>` string on the wire (JSON) and at
//! rest (TEXT columns). The newtype wrapper keeps the prefix discipline and makes it
//! a compile error to pass one id kind where another is expected. The inner `String`
//! is private; construct with [`fresh`](MessageId::fresh) or `from`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Declares a prefixed-string id newtype with a fresh-id generator and the standard
/// conversions. Kept here (not exported) so every id kind is identical by construction.
macro_rules! id_newtype {
    ($prefix:literal, $(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// The textual prefix carried by every value of this id kind.
            pub const PREFIX: &'static str = $prefix;

            /// Generate a fresh, unique id of the form `<prefix>_<uuid-simple>`.
            #[must_use]
            pub fn fresh() -> Self {
                Self(format!("{}_{}", $prefix, Uuid::new_v4().simple()))
            }

            /// Borrow the underlying string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume the id, returning the owned string.
            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_newtype!(
    "msg",
    #[doc = "Internal message id (`msg_…`)."]
    MessageId
);
id_newtype!(
    "thread",
    #[doc = "Internal thread id (`thread_…`)."]
    ThreadId
);
id_newtype!(
    "draft",
    #[doc = "Operational draft-record id (`draft_…`)."]
    DraftId
);
id_newtype!(
    "acct",
    #[doc = "Mail-account identifier (`acct_…`), adapter-provided."]
    AccountId
);
id_newtype!(
    "folder",
    #[doc = "Mail-folder identifier (`folder_…`), adapter-provided."]
    FolderId
);
id_newtype!(
    "sender",
    #[doc = "Sender-profile id (`sender_…`)."]
    SenderId
);
id_newtype!(
    "dec",
    #[doc = "Policy/planning decision id (`dec_…`)."]
    DecisionId
);
id_newtype!(
    "rule",
    #[doc = "Rule id (`rule_…`)."]
    RuleId
);
id_newtype!(
    "rv",
    #[doc = "Immutable rule-version id (`rv_…`)."]
    RuleVersionId
);
id_newtype!(
    "mf",
    #[doc = "Message-feature row id (`mf_…`)."]
    MessageFeatureId
);
id_newtype!(
    "fb",
    #[doc = "Per-task feedback-row id (`fb_…` by default; the per-table prefixes \
             `clsfb_`/`filfb_`/… are minted via [`fresh_prefixed`])."]
    FeedbackId
);
id_newtype!(
    "audit",
    #[doc = "Append-only audit-log entry id (`audit_…`)."]
    AuditId
);
id_newtype!(
    "prop",
    #[doc = "Agent rule-proposal id (`prop_…`)."]
    ProposalId
);
id_newtype!(
    "evid",
    #[doc = "Rule-evidence row id (`evid_…`)."]
    EvidenceId
);
id_newtype!(
    "shad",
    #[doc = "Shadow-outcome row id (`shad_…`)."]
    ShadowOutcomeId
);

/// Mint a fresh `<prefix>_<uuid-simple>` string with an explicit prefix.
///
/// Most ids carry their prefix in the newtype (via [`fresh`](MessageId::fresh)). The
/// per-task feedback tables are the exception: they all surface as one [`FeedbackId`]
/// domain type, yet each table stamps a finer-grained, human-readable row prefix
/// (`clsfb_`, `filfb_`, …) so a raw DB row announces which table it came from. Those ids
/// are minted here from the kind's `ID_PREFIX`.
#[must_use]
pub fn fresh_prefixed(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_carries_its_prefix_and_is_unique() {
        let a = MessageId::fresh();
        let b = MessageId::fresh();
        assert!(a.as_str().starts_with("msg_"), "got {a}");
        assert_eq!(MessageId::PREFIX, "msg");
        assert_ne!(a, b, "two fresh ids must differ");
    }

    #[test]
    fn round_trips_through_serde_as_a_bare_string() {
        let id = FolderId::from("folder_inbox");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"folder_inbox\"");
        let back: FolderId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn distinct_id_kinds_do_not_unify() {
        // A compile-time guarantee in spirit; here we assert the value-level contract
        // that conversions are explicit and the inner string is preserved.
        let m = MessageId::from("msg_1");
        assert_eq!(m.into_string(), "msg_1");
    }

    #[test]
    fn phase2_id_kinds_carry_their_prefixes() {
        assert_eq!(SenderId::PREFIX, "sender");
        assert!(SenderId::fresh().as_str().starts_with("sender_"));
        assert_eq!(MessageFeatureId::PREFIX, "mf");
        assert!(MessageFeatureId::fresh().as_str().starts_with("mf_"));
        assert_eq!(DecisionId::PREFIX, "dec");
        assert!(DecisionId::fresh().as_str().starts_with("dec_"));
        assert_eq!(RuleId::PREFIX, "rule");
        assert_eq!(RuleVersionId::PREFIX, "rv");
        assert!(RuleVersionId::fresh().as_str().starts_with("rv_"));
    }

    #[test]
    fn phase7_id_kinds_carry_their_prefixes() {
        assert_eq!(FeedbackId::PREFIX, "fb");
        assert_eq!(AuditId::PREFIX, "audit");
        assert!(AuditId::fresh().as_str().starts_with("audit_"));
        assert_eq!(ProposalId::PREFIX, "prop");
        assert!(ProposalId::fresh().as_str().starts_with("prop_"));
        assert_eq!(EvidenceId::PREFIX, "evid");
        assert!(EvidenceId::fresh().as_str().starts_with("evid_"));
        assert_eq!(ShadowOutcomeId::PREFIX, "shad");
        assert!(ShadowOutcomeId::fresh().as_str().starts_with("shad_"));
    }

    #[test]
    fn fresh_prefixed_honours_the_per_table_feedback_prefix() {
        let raw = fresh_prefixed("clsfb");
        assert!(raw.starts_with("clsfb_"), "got {raw}");
        let id = FeedbackId::from(raw);
        assert!(id.as_str().starts_with("clsfb_"));
        assert_ne!(fresh_prefixed("filfb"), fresh_prefixed("filfb"), "unique");
    }
}
