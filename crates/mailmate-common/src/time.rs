//! Time on the wire and at rest: RFC 3339 / ISO-8601, UTC.

use serde::{Deserialize, Serialize};

/// A point in time, serialized as an RFC 3339 string (`2026-06-14T03:21:09Z`).
///
/// Wrapping `time::OffsetDateTime` keeps a single, explicit time representation across
/// the wire, storage, and the scheduler — and lets a `Clock` adapter (real or fake)
/// be the only source of "now".
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Timestamp(#[serde(with = "time::serde::rfc3339")] pub time::OffsetDateTime);

impl Timestamp {
    /// The current wall-clock instant in UTC.
    ///
    /// Production code should prefer the injected `Clock` port so tests stay
    /// deterministic; this is the convenience the system-clock adapter is built on.
    #[must_use]
    pub fn now() -> Self {
        Self(time::OffsetDateTime::now_utc())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn serializes_as_an_rfc3339_string() {
        let ts = Timestamp(datetime!(2026-06-14 03:21:09 UTC));
        let json = serde_json::to_string(&ts).unwrap();
        assert_eq!(json, "\"2026-06-14T03:21:09Z\"");
        let back: Timestamp = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ts);
    }

    #[test]
    fn now_is_monotonic_non_decreasing() {
        let a = Timestamp::now();
        let b = Timestamp::now();
        assert!(b >= a);
    }
}
