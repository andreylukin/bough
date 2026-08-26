//! §0.2 runtime invariant for `bough-plugin-workers`:
//!
//! **Live runs never exceed `max_in_flight`, no run exceeds `max_depth`, and every
//! `worker/report` has a `worker/started` before it for the same worker.**
//!
//! The first two are a fold over the run registry, the third over the observed `ledger/step`
//! stream. WP-6 owns the recorder and the check.

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};

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

/// Record one moment. WP-6.
pub fn record(_obs: Obs) {
    todo!("WP-6")
}

/// Forget everything recorded for `fiber`. WP-6.
pub fn forget(_fiber: FiberUid) {
    todo!("WP-6")
}

/// Everything recorded so far, oldest first. WP-6.
pub fn seen() -> Vec<Obs> {
    todo!("WP-6")
}

/// The whole invariant as a pure function of the observed stream and the configured bounds. WP-6.
pub fn evaluate(_bounds: &Bounds, _stream: &[Obs]) -> Result<(), String> {
    todo!("WP-6: in-flight, depth, report-after-start")
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

async fn check(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-6: read the configured bounds and evaluate the recorded stream")
}
