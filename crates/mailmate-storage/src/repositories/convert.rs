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
