//! The production [`Clock`]: the system wall-clock.
//!
//! Every scheduled-time decision in MailMate flows through the `Clock` port so tests can pin
//! "now"; this is the one adapter that actually reads the wall-clock, built on
//! [`Timestamp::now`]. It is injected at the composition root and nowhere else.

use mailmate_common::time::Timestamp;
use mailmate_ports::clock::Clock;

/// The system wall-clock `Clock` adapter.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl SystemClock {
    /// Construct the system clock.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_monotonic_non_decreasing() {
        let clock = SystemClock::new();
        let a = clock.now();
        let b = clock.now();
        assert!(b >= a, "wall-clock now() must not go backwards");
    }
}
