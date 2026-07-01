//! Small, shared row-conversion helpers used by the repository impls — timestamp and
//! opaque-JSON column (de)serialization, kept in one place so every repository converts
//! identically.

use mailmate_common::error::StorageError;
use mailmate_common::time::Timestamp;

/// Render a timestamp for a `TEXT` column.
pub(crate) fn ts_to_db(ts: Timestamp) -> String {
    ts.to_rfc3339()
}

/// Parse a timestamp read back from a `TEXT` column.
pub(crate) fn ts_from_db(s: &str) -> Result<Timestamp, StorageError> {
    Timestamp::parse_rfc3339(s).map_err(|e| StorageError::Serialization(e.to_string()))
}

/// Serialize a value to the opaque text held in a `*_json` column.
pub(crate) fn json_to_db<T: serde::Serialize>(value: &T) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|e| StorageError::Serialization(e.to_string()))
}

/// Deserialize the opaque text of a `*_json` column back into a value.
pub(crate) fn json_from_db<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, StorageError> {
    serde_json::from_str(s).map_err(|e| StorageError::Serialization(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_round_trips_through_the_text_column() {
        let ts = Timestamp::parse_rfc3339("2026-06-22T10:30:00Z").unwrap();
        let rendered = ts_to_db(ts);
        assert_eq!(ts_from_db(&rendered).unwrap(), ts);
    }

    #[test]
    fn a_malformed_timestamp_string_maps_to_a_serialization_error() {
        let err = ts_from_db("not-a-timestamp").unwrap_err();
        assert!(matches!(err, StorageError::Serialization(_)));
    }

    #[test]
    fn json_round_trips_through_the_opaque_column() {
        let value = vec!["a".to_string(), "b".to_string()];
        let text = json_to_db(&value).unwrap();
        let back: Vec<String> = json_from_db(&text).unwrap();
        assert_eq!(back, value);
    }

    #[test]
    fn malformed_json_maps_to_a_serialization_error() {
        let err = json_from_db::<Vec<String>>("{ this is not json").unwrap_err();
        assert!(matches!(err, StorageError::Serialization(_)));
    }
}
