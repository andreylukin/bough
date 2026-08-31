//! §0.2 runtime invariant for `bough-plugin-workers`:
//!
//! **Live runs never exceed `max_in_flight`, no run exceeds `max_depth`, and every
//! `worker/report` has a `worker/started` before it for the same worker.**
//!
//! The whole check is a pure function of an observed stream plus the configured bounds
//! ([`evaluate`]), exactly as the ledger's four are. The stream is recorded per fiber LIFE and
//! forgotten by an inverse the row's `apply` registers, because a RELOAD keeps the `FiberUid`.
//!
//! Cadence is [`Cadence::OnQuiesce`] (P1-D14): `Interval`/`OnEvent` are still undispatched.

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use parking_lot::Mutex;
use std::collections::BTreeSet;

use crate::ids::WorkerId;
use crate::start::Bounds;

/// One observed moment of a run's life.
#[derive(Clone, Debug, PartialEq)]
pub enum Obs {
    Started {
        fiber: FiberUid,
        worker: WorkerId,
        depth: u8,
        in_flight_after: usize,
    },
    Reported {
        fiber: FiberUid,
        worker: WorkerId,
    },
    Finished {
        fiber: FiberUid,
        worker: WorkerId,
    },
}

impl Obs {
    fn fiber(&self) -> FiberUid {
        match self {
            Obs::Started { fiber, .. }
            | Obs::Reported { fiber, .. }
            | Obs::Finished { fiber, .. } => *fiber,
        }
    }
}

/// The recorded stream. A run's life is a handful of observations and runs are bounded, so this
/// is a transcript rather than a fold: the report-after-start relation needs the order.
static SEEN: Mutex<Vec<Obs>> = Mutex::new(Vec::new());
/// The bounds the mounted row was configured with, so the check reads the SAME numbers `start`
/// enforced instead of a second copy.
static BOUNDS: Mutex<Option<Bounds>> = Mutex::new(None);

/// Record one moment.
pub fn record(obs: Obs) {
    SEEN.lock().push(obs);
}

/// Forget everything recorded for `fiber`.
pub fn forget(fiber: FiberUid) {
    SEEN.lock().retain(|o| o.fiber() != fiber);
}

/// Everything recorded so far, oldest first.
pub fn seen() -> Vec<Obs> {
    SEEN.lock().clone()
}

/// Publish the mounted row's bounds. Called by `apply`.
pub fn set_bounds(b: Bounds) {
    *BOUNDS.lock() = Some(b);
}

/// The configured bounds, if a row is mounted.
pub fn bounds() -> Option<Bounds> {
    BOUNDS.lock().clone()
}

/// The whole invariant as a pure function of the observed stream and the configured bounds.
pub fn evaluate(bounds: &Bounds, stream: &[Obs]) -> Result<(), String> {
    let mut started: BTreeSet<WorkerId> = BTreeSet::new();
    for obs in stream {
        match obs {
            Obs::Started {
                worker,
                depth,
                in_flight_after,
                ..
            } => {
                if *in_flight_after > bounds.max_in_flight {
                    return Err(format!(
                        "worker `{worker}` started with {in_flight_after} runs in flight; \
                         max_in_flight is {}",
                        bounds.max_in_flight
                    ));
                }
                if *depth as usize > bounds.max_depth as usize {
                    return Err(format!(
                        "worker `{worker}` started at depth {depth}; max_depth is {}",
                        bounds.max_depth
                    ));
                }
                started.insert(worker.clone());
            }
            Obs::Reported { worker, .. } => {
                if !started.contains(worker) {
                    return Err(format!(
                        "worker `{worker}` reported without ever having started"
                    ));
                }
            }
            Obs::Finished { worker, .. } => {
                started.remove(worker);
            }
        }
    }
    Ok(())
}

/// The spec `WorkersPlugin::invariants` returns.
pub fn runs_stay_within_bounds() -> InvariantSpec {
    InvariantSpec {
        name: "worker_runs_stay_within_bounds_and_report_after_starting",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    // No mounted row ⇒ nothing to be within: an unmounted seam is not a violation.
    let Some(bounds) = bounds() else {
        return Ok(());
    };
    evaluate(&bounds, &seen()).map_err(|detail| InvariantViolation {
        invariant: "worker_runs_stay_within_bounds_and_report_after_starting",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> Bounds {
        Bounds {
            max_in_flight: 2,
            max_depth: 2,
            per_wake_spawn_cap: 2,
        }
    }
    /// Distinct uids must come from ONE core: two fresh cores hand out the same first uid.
    fn two_fibers() -> (FiberUid, FiberUid) {
        let core = bough_kernel::KernelCore::new();
        (core.new_fiber_uid(), core.new_fiber_uid())
    }
    fn f() -> FiberUid {
        bough_kernel::KernelCore::new().new_fiber_uid()
    }
    fn started(w: &str, depth: u8, in_flight: usize) -> Obs {
        Obs::Started {
            fiber: f(),
            worker: WorkerId::new(w),
            depth,
            in_flight_after: in_flight,
        }
    }

    #[test]
    fn a_clean_stream_passes() {
        let s = vec![
            started("a", 1, 1),
            started("b", 2, 2),
            Obs::Reported {
                fiber: f(),
                worker: WorkerId::new("a"),
            },
            Obs::Finished {
                fiber: f(),
                worker: WorkerId::new("a"),
            },
        ];
        evaluate(&bounds(), &s).expect("clean");
    }

    #[test]
    fn too_many_in_flight_is_a_violation_naming_the_bound() {
        let e = evaluate(&bounds(), &[started("a", 1, 3)]).expect_err("3 > 2");
        assert!(e.contains("max_in_flight"), "{e}");
    }

    #[test]
    fn too_deep_is_a_violation_naming_the_bound() {
        let e = evaluate(&bounds(), &[started("a", 3, 1)]).expect_err("depth 3 > 2");
        assert!(e.contains("max_depth"), "{e}");
    }

    #[test]
    fn a_report_from_a_worker_that_never_started_is_a_violation() {
        let e = evaluate(
            &bounds(),
            &[Obs::Reported {
                fiber: f(),
                worker: WorkerId::new("ghost"),
            }],
        )
        .expect_err("no start");
        assert!(e.contains("ghost"), "{e}");
    }

    /// The record is per fiber LIFE: another fiber's observations survive a forget.
    #[test]
    fn forget_drops_only_that_fibers_observations() {
        let (a, b) = two_fibers();
        record(Obs::Finished {
            fiber: a,
            worker: WorkerId::new("fa"),
        });
        record(Obs::Finished {
            fiber: b,
            worker: WorkerId::new("fb"),
        });
        forget(a);
        let left = seen();
        assert!(!left.iter().any(|o| o.fiber() == a));
        assert!(left.iter().any(|o| o.fiber() == b));
        forget(b);
    }
}
