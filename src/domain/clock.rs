//! Clock abstraction, so timer logic is deterministic under test.
//!
//! Nothing in [`crate::domain`] reads the wall clock directly. Time arrives as
//! a parameter or through this trait, which lets a test advance an hour
//! instantly instead of sleeping.
//!
//! # Example
//!
//! ```
//! use chrono::Duration;
//! use pugbot::domain::clock::{Clock, FakeClock};
//!
//! let clock = FakeClock::at_epoch();
//! let start = clock.now();
//! clock.advance(Duration::hours(2));
//! assert_eq!(clock.now() - start, Duration::hours(2));
//! ```

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};

/// A source of the current time.
///
/// Implementors must be cheap to call: services read the clock several times
/// per command.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// The current instant, in UTC.
    fn now(&self) -> DateTime<Utc>;
}

/// The real clock, used in every non-test build.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Manually advanced clock for unit tests.
#[derive(Debug, Clone)]
pub struct FakeClock {
    now: Arc<Mutex<DateTime<Utc>>>,
}

impl FakeClock {
    /// Creates a clock stopped at `start`.
    #[must_use]
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            now: Arc::new(Mutex::new(start)),
        }
    }

    /// A fixed, arbitrary instant used as the default test epoch.
    ///
    /// # Panics
    ///
    /// Never: the timestamp is a compile-time literal known to be valid.
    #[must_use]
    pub fn at_epoch() -> Self {
        Self::new(
            DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .expect("valid literal")
                .with_timezone(&Utc),
        )
    }

    /// Moves the clock forward by `by`.
    ///
    /// # Panics
    ///
    /// Panics if another thread panicked while holding the internal lock.
    pub fn advance(&self, by: Duration) {
        let mut guard = self.now.lock().expect("clock mutex poisoned");
        *guard += by;
    }

    /// Moves the clock to an absolute instant, forward or backward.
    ///
    /// # Panics
    ///
    /// Panics if another thread panicked while holding the internal lock.
    pub fn set(&self, to: DateTime<Utc>) {
        *self.now.lock().expect("clock mutex poisoned") = to;
    }
}

impl Clock for FakeClock {
    /// # Panics
    ///
    /// Panics if another thread panicked while holding the internal lock.
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().expect("clock mutex poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_clock_advances_only_when_told() {
        let clock = FakeClock::at_epoch();
        let first = clock.now();
        assert_eq!(first, clock.now());
        clock.advance(Duration::seconds(90));
        assert_eq!(clock.now() - first, Duration::seconds(90));
    }
}
