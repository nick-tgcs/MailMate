//! The clock port: the single source of "now".

use mailmate_common::time::Timestamp;

/// The source of the current time.
///
/// The system wall-clock is the default adapter; a fake, settable clock makes the
/// follow-up scheduler and crystallization back-tests fully deterministic. Time is a
/// port precisely so no core logic reaches for `OffsetDateTime::now_utc()` directly.
pub trait Clock: Send + Sync {
    /// The current instant, per this clock.
    fn now(&self) -> Timestamp;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn Clock) {}
        let _ = takes as fn(&dyn Clock);
    }
}
