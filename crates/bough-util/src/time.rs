//! Invariant: a timeout is a value, not a hidden constant. `with_timeout` and `Deadline` take the
//! duration from the caller so a deployment-varying wait is always a validated `Config` field in
//! some plugin row, never a literal buried in a helper (§0.2 "no hardcoded tunables").

use std::future::Future;
use std::time::{Duration, Instant};

/// The error `with_timeout` returns; carries the duration that was actually waited.
#[derive(Debug, thiserror::Error)]
#[error("timed out after {0:?}")]
pub struct TimedOut(pub Duration);

/// Await `f`, giving up after `d`. The runtime is the caller's: this creates none.
pub async fn with_timeout<T>(d: Duration, f: impl Future<Output = T>) -> Result<T, TimedOut> {
    let started = Instant::now();
    match tokio::time::timeout(d, f).await {
        Ok(v) => Ok(v),
        Err(_) => Err(TimedOut(started.elapsed())),
    }
}

/// An absolute instant a caller must finish by.
#[derive(Clone, Copy, Debug)]
pub struct Deadline(Instant);

impl Deadline {
    /// A deadline `d` from now.
    pub fn in_(d: Duration) -> Self {
        Self(Instant::now() + d)
    }
    /// Time left, or `None` once expired.
    pub fn remaining(&self) -> Option<Duration> {
        self.0
            .checked_duration_since(Instant::now())
            .filter(|r| !r.is_zero())
    }
    /// Whether the deadline has passed.
    pub fn expired(&self) -> bool {
        self.remaining().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn with_timeout_returns_the_value() {
        let v = with_timeout(Duration::from_secs(30), async { 7u32 })
            .await
            .unwrap();
        assert_eq!(v, 7);
    }

    #[tokio::test]
    async fn with_timeout_reports_the_duration_it_waited() {
        let budget = Duration::from_millis(50);
        let err = with_timeout(budget, tokio::time::sleep(Duration::from_secs(60)))
            .await
            .expect_err("the inner future outlives the budget");
        assert!(err.0 >= budget, "waited {:?}, budget {:?}", err.0, budget);
        assert!(err.to_string().contains("timed out after"));
    }

    #[test]
    fn deadline_expires() {
        let d = Deadline::in_(Duration::from_millis(0));
        assert!(d.expired());
        assert_eq!(d.remaining(), None);

        let later = Deadline::in_(Duration::from_secs(60));
        assert!(!later.expired());
        assert!(later.remaining().unwrap() <= Duration::from_secs(60));
    }
}
