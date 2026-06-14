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
}
