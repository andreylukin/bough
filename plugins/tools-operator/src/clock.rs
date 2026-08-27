//! Invariant: nothing in this row reads a global clock. `now` is injected, so a scheduled intent
//! can be tested by advancing a synthetic clock rather than by sleeping.

use chrono::{DateTime, Utc};

/// The injected clock.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
}

/// Production.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
