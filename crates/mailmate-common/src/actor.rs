//! Who caused a recorded fact. The same small vocabulary tags an `audit_log` row's
//! origin and a rule/version's `created_by`, so provenance reads identically across the
//! audit timeline and the rule tables.

use serde::{Deserialize, Serialize};

/// The origin of a recorded action or artifact.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    /// A human user (a correction, an approval, a manual move).
    #[default]
    User,
    /// The MailMate host itself (a background decision, a lifecycle transition).
    System,
    /// An AI provider or the learning engine acting as teacher (a proposal).
    Ai,
    /// The Thunderbird extension (a UI-driven event relayed to the host).
    Extension,
    /// A bulk import (rules/data brought in from elsewhere).
    Import,
}

impl Actor {
    /// The stable snake_case label stored in a `TEXT` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::System => "system",
            Self::Ai => "ai",
            Self::Extension => "extension",
            Self::Import => "import",
        }
    }

    /// Parse a stored label back into an [`Actor`], or `None` if unrecognized.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "system" => Some(Self::System),
            "ai" => Some(Self::Ai),
            "extension" => Some(Self::Extension),
            "import" => Some(Self::Import),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_its_db_label() {
        for actor in [
            Actor::User,
            Actor::System,
            Actor::Ai,
            Actor::Extension,
            Actor::Import,
        ] {
            assert_eq!(Actor::from_db_str(actor.as_str()), Some(actor));
        }
        assert_eq!(Actor::from_db_str("nope"), None);
        assert_eq!(Actor::default(), Actor::User);
    }
}
