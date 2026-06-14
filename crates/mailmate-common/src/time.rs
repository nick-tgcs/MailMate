//! Time on the wire and at rest: RFC 3339 / ISO-8601, UTC.

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;

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

    /// Render as the RFC 3339 string stored in `TEXT` timestamp columns.
    ///
    /// Formatting a UTC `OffsetDateTime` as RFC 3339 cannot fail, so this does not return
    /// a `Result` — the single `expect` documents that invariant.
    #[must_use]
    pub fn to_rfc3339(self) -> String {
        self.0
            .format(&Rfc3339)
            .expect("formatting a UTC OffsetDateTime as RFC 3339 is infallible")
    }

    /// Parse the RFC 3339 string read back from a `TEXT` timestamp column.
    ///
    /// # Errors
    /// [`time::error::Parse`] if `s` is not a valid RFC 3339 timestamp.
    pub fn parse_rfc3339(s: &str) -> Result<Self, time::error::Parse> {
        time::OffsetDateTime::parse(s, &Rfc3339).map(Self)
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

    #[test]
    fn db_string_round_trips() {
        let ts = Timestamp(datetime!(2026-06-14 03:21:09 UTC));
        let s = ts.to_rfc3339();
        assert_eq!(s, "2026-06-14T03:21:09Z");
        assert_eq!(Timestamp::parse_rfc3339(&s).unwrap(), ts);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(Timestamp::parse_rfc3339("not-a-timestamp").is_err());
    }
}
