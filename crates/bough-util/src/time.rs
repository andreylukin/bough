//! Invariant: a timeout is a value, not a hidden constant. `with_timeout` and `Deadline` take the
//! duration from the caller so a deployment-varying wait is always a validated `Config` field in
//! some plugin row, never a literal buried in a helper (§0.2 "no hardcoded tunables").

use std::future::Future;
use std::time::{Duration, Instant};

/// The error `with_timeout` returns; carries the duration that was actually waited.
#[derive(Debug, thiserror::Error)]
#[error("timed out after {0:?}")]
pub struct TimedOut(pub Duration);

/// Await `f`, giving up after `d`.
pub async fn with_timeout<T>(d: Duration, f: impl Future<Output = T>) -> Result<T, TimedOut> {
    let _ = (d, f);
    todo!("WP-1: race the future against a sleep; the runtime is the caller's")
}

/// An absolute instant a caller must finish by.
#[derive(Clone, Copy, Debug)]
pub struct Deadline(#[allow(dead_code)] Instant); // SCAFFOLD: allow goes when WP-1 fills the bodies

impl Deadline {
    /// A deadline `d` from now.
    pub fn in_(d: Duration) -> Self {
        let _ = d;
        todo!("WP-1")
    }
    /// Time left, or `None` once expired.
    pub fn remaining(&self) -> Option<Duration> {
        todo!("WP-1")
    }
    /// Whether the deadline has passed.
    pub fn expired(&self) -> bool {
        self.remaining().is_none()
    }
}
