//! §0.2 runtime invariant for `bough-plugin-power`:
//!
//! **The mounted source's `last()` is the last payload that went through the seam.** A source that
//! dispatched an event it does not itself remember is the violation: `catch-up-on-wake` acts on
//! the event and every other reader (`/power`, the swap test, a log line) reads `last()`, so the
//! two disagreeing means one of them is lying about what the machine did.
//!
//! Nothing here checks ORDERING. A `DidWake` with no preceding `WillSleep` is a real thing on a
//! real laptop — a dark wake, a process started while the lid was already closing — and an
//! invariant that called it a violation would fire on correct behaviour.

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};

use crate::{Power, PowerEvent};

/// One dispatched payload.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    /// The DISPATCHING fiber — the Provider's, not the Definition's.
    pub fiber: FiberUid,
    pub event: PowerEvent,
}

static SEEN: parking_lot::Mutex<Vec<Obs>> = parking_lot::Mutex::new(Vec::new());

/// How many observations the record keeps. A sleep/wake pair a day for a year is under this; the
/// bound exists because an unbounded record is a leak rather than a check.
const CAP: usize = 1024;

/// Record one dispatched payload. Called by [`crate::dispatch`], which every Provider goes through.
pub fn record(obs: Obs) {
    let mut seen = SEEN.lock();
    seen.push(obs);
    let len = seen.len();
    if len > CAP {
        seen.drain(0..len - CAP);
    }
}

/// Forget everything recorded by `fiber`, as an inverse of that fiber's `apply`.
pub fn forget(fiber: FiberUid) {
    SEEN.lock().retain(|o| o.fiber != fiber);
}

/// Everything recorded so far, oldest first.
pub fn seen() -> Vec<Obs> {
    SEEN.lock().clone()
}

/// Drop the record. Test setup only.
pub fn clear() {
    SEEN.lock().clear();
}

/// PURE: the comparison the check runs. An empty stream is clean — nothing has happened yet.
pub fn check_last(seen: &[Obs], last: Option<PowerEvent>) -> Result<(), String> {
    let Some(newest) = seen.last() else {
        return Ok(());
    };
    match last {
        Some(l) if l == newest.event => Ok(()),
        Some(l) => Err(format!(
            "the source reports `{}` as its last event but `{}` went through the seam",
            l.kind(),
            newest.event.kind()
        )),
        None => Err(format!(
            "a `{}` went through the seam but the source remembers nothing",
            newest.event.kind()
        )),
    }
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "the_sources_last_event_is_the_last_one_dispatched",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    // No Provider mounted is not a violation of THIS row: `sleep-listener` is the row §0.2 holds
    // to activating, and it has its own check.
    let Ok(Some(power)) = ctx.try_get::<Power>() else {
        return Ok(());
    };
    check_last(&seen(), power.0.last()).map_err(|detail| InvariantViolation {
        invariant: "the_sources_last_event_is_the_last_one_dispatched",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn fiber() -> FiberUid {
        bough_kernel::KernelCore::new().new_fiber_uid()
    }

    fn sleep_obs(f: FiberUid) -> Obs {
        Obs {
            fiber: f,
            event: PowerEvent::WillSleep {
                at: chrono::Utc::now(),
            },
        }
    }

    fn wake_obs(f: FiberUid) -> Obs {
        Obs {
            fiber: f,
            event: PowerEvent::DidWake {
                at: chrono::Utc::now(),
                asleep_for: Some(Duration::from_secs(120)),
            },
        }
    }

    #[test]
    fn an_empty_stream_is_clean() {
        assert_eq!(check_last(&[], None), Ok(()));
    }

    #[test]
    fn the_newest_payload_must_be_what_the_source_remembers() {
        let f = fiber();
        let (s, w) = (sleep_obs(f), wake_obs(f));
        assert_eq!(
            check_last(&[s.clone(), w.clone()], Some(w.event.clone())),
            Ok(())
        );
        let detail = check_last(&[s.clone(), w], Some(s.event))
            .expect_err("a stale `last()` must be reported");
        assert!(detail.contains("did-wake"), "{detail}");
    }

    #[test]
    fn a_source_that_remembers_nothing_after_dispatching_is_a_violation() {
        let f = fiber();
        let detail = check_last(&[wake_obs(f)], None).expect_err("must be reported");
        assert!(detail.contains("remembers nothing"), "{detail}");
    }

    #[test]
    fn forget_drops_only_that_fibers_rows() {
        clear();
        let (a, b) = (fiber(), fiber());
        record(sleep_obs(a));
        record(wake_obs(b));
        forget(a);
        let left = seen();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].fiber, b);
        clear();
    }
}
