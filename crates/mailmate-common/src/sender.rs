//! The sender-profile entity: sender-level learning without storing raw personal data
//! by default. Aggregate features live in an opaque `feature_json` value.

use serde::{Deserialize, Serialize};

use crate::ids::SenderId;
use crate::time::Timestamp;

/// How much a sender is trusted, governing how aggressive an action may be.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// Not yet assessed.
    #[default]
    Unknown,
    /// Known-good sender.
    Trusted,
    /// Treat with extra caution.
    Suspicious,
    /// Known-bad sender.
    Blocked,
}

impl TrustLevel {
    /// The stable lower-snake token stored in the `trust_level` column.
    #[must_use]
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Trusted => "trusted",
            Self::Suspicious => "suspicious",
            Self::Blocked => "blocked",
        }
    }

    /// Parse the value read back from the `trust_level` column.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "unknown" => Some(Self::Unknown),
            "trusted" => Some(Self::Trusted),
            "suspicious" => Some(Self::Suspicious),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }
}

/// A persisted sender profile.
///
/// `feature_json` is an opaque aggregate-feature object written/read whole by Rust; it is
/// a [`serde_json::Value`], so this type derives `PartialEq` but not `Eq`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SenderProfile {
    /// Internal `sender_…` id.
    pub id: SenderId,
    /// Readable email address.
    pub email: String,
    /// Readable domain.
    pub domain: String,
    /// Readable display name, if known.
    pub display_name: Option<String>,
    /// Trust assessment.
    pub trust_level: TrustLevel,
    /// When the sender was most recently seen.
    pub last_seen_at: Timestamp,
    /// Opaque aggregate features.
    pub feature_json: serde_json::Value,
    /// Insert timestamp.
    pub created_at: Timestamp,
    /// Last-update timestamp.
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn trust_level_db_strings_round_trip() {
        for level in [
            TrustLevel::Unknown,
            TrustLevel::Trusted,
            TrustLevel::Suspicious,
            TrustLevel::Blocked,
        ] {
            assert_eq!(TrustLevel::from_db_str(level.as_db_str()), Some(level));
        }
        assert_eq!(TrustLevel::from_db_str("???"), None);
        assert_eq!(TrustLevel::default(), TrustLevel::Unknown);
    }

    #[test]
    fn sender_profile_round_trips_through_serde() {
        let profile = SenderProfile {
            id: SenderId::from("sender_1"),
            email: "s@example.com".to_owned(),
            domain: "example.com".to_owned(),
            display_name: Some("Sam".to_owned()),
            trust_level: TrustLevel::Trusted,
            last_seen_at: Timestamp::now(),
            feature_json: json!({ "reply_rate": 0.8 }),
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        };
        let json = serde_json::to_string(&profile).unwrap();
        let back: SenderProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back, profile);
    }
}
