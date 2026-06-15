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

    /// This instant shifted by `days` whole days (negative shifts backward). The follow-up
    /// cadence counts absolute day-offsets from an anchor, so this is the one place the
    /// scheduler needs date arithmetic — kept here beside the time representation rather
    /// than reaching for `time::Duration` in an adapter.
    #[must_use]
    pub fn add_days(self, days: i64) -> Self {
        Self(self.0 + time::Duration::days(days))
    }

    /// The number of whole days from `earlier` to `self` (negative if `self` precedes
    /// `earlier`). Used by the staleness guard (`now − due_time ≤ horizon`).
    #[must_use]
    pub fn whole_days_since(self, earlier: Self) -> i64 {
        (self.0 - earlier.0).whole_days()
    }

    /// This instant with its sub-second component dropped (floored to the whole second).
    ///
    /// RFC 3339 renders sub-seconds only when non-zero, so a whole-second value
    /// (`…00Z`) and a fractional value (`…00.5Z`) within the same second do **not** order
    /// lexicographically the way they order chronologically. The follow-up scheduler stores
    /// and compares its day-granular `next_due_at` at whole-second precision (via this
    /// helper) so the `next_due_at <= now` TEXT comparison is exact.
    #[must_use]
    pub fn floor_to_seconds(self) -> Self {
        Self(
            self.0
                .replace_nanosecond(0)
                .expect("zero nanoseconds is always in range"),
        )
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

    #[test]
    fn add_days_and_whole_days_since_are_inverse() {
        let anchor = Timestamp(datetime!(2026-06-01 00:00:00 UTC));
        let due = anchor.add_days(14);
        assert_eq!(due.to_rfc3339(), "2026-06-15T00:00:00Z");
        assert_eq!(due.whole_days_since(anchor), 14);
        assert_eq!(anchor.whole_days_since(due), -14);
        // A partial day floors toward zero.
        let plus_partial = anchor.add_days(3);
        let now = Timestamp(datetime!(2026-06-04 12:00:00 UTC));
        assert_eq!(now.whole_days_since(plus_partial), 0, "12h is <1 whole day");
    }

    #[test]
    fn floor_to_seconds_makes_text_order_match_instant_order() {
        // A fractional and a whole-second value in the same second DON'T order the same way
        // lexicographically vs chronologically; flooring both fixes it for TEXT comparison.
        let frac =
            Timestamp(datetime!(2026-06-20 00:00:00 UTC) + time::Duration::milliseconds(500));
        let whole = Timestamp(datetime!(2026-06-20 00:00:00 UTC));
        assert!(whole.0 <= frac.0, "chronologically whole precedes frac");
        assert!(
            whole.to_rfc3339() > frac.to_rfc3339(),
            "but lexicographically the whole-second string sorts AFTER the fractional one"
        );
        // Floored, both render at whole-second precision and the TEXT order is exact.
        assert_eq!(frac.floor_to_seconds().to_rfc3339(), "2026-06-20T00:00:00Z");
        assert!(whole.floor_to_seconds().to_rfc3339() <= frac.floor_to_seconds().to_rfc3339());
    }
}
