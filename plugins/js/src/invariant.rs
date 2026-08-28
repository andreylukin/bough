//! §0.2 runtime invariant for `bough-plugin-js`:
//!
//! **A cancelled program never reports a `Run`, and a `Run` never claims a cost past its caps.**
//!
//! The wider claim this file used to make — "exactly one terminal outcome, never both and never
//! neither" — is enforced by the TYPE: [`crate::JsHandle::run`] returns one `Result<Run, JsError>`,
//! so "both" and "neither" are unrepresentable, and a check for them could never fire whatever an
//! engine did. Two of the three clauses were therefore unfalsifiable at runtime and their unit
//! tests proved a predicate over `Obs` values the seam cannot construct. They are gone.
//!
//! What is left are the two clauses an engine really can get wrong, each checked against a SECOND
//! observation the outcome does not determine: the cancellation token's state when the engine
//! answered (a cancelled program that still reported output), and the cost the `Run` itself
//! claims against the caps it was given (a cap breach dressed up as a clean answer, which the
//! consumer would ledger as the round's result). Both can fail at runtime.

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};

/// One finished program, as the seam saw it.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    /// A digest of the source, so a violation names the program it belongs to.
    pub program: String,
    /// Did the engine report a `Run`?
    pub ran: bool,
    /// Did the engine report a `JsError`?
    pub errored: bool,
    /// Was the program's cancellation token tripped?
    pub cancelled: bool,
    /// The cost the engine reported on a `Run`, and the caps it ran under. `None` when the
    /// program ended in a `JsError`, which is where a breach belongs.
    pub cost: Option<Cost>,
}

/// What a `Run` claimed to have spent, against what it was allowed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cost {
    pub ops: u64,
    pub ms: u64,
    pub ops_cap: u64,
    pub wall_ms: u64,
}

static SEEN: parking_lot::Mutex<Vec<Obs>> = parking_lot::Mutex::new(Vec::new());

/// Record one finished program. Called by the seam when the engine returns.
pub fn record(obs: Obs) {
    SEEN.lock().push(obs);
}

/// Forget everything recorded for `fiber` (registered as an inverse by `apply`).
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

/// The whole invariant as a pure function of the observed programs.
pub fn evaluate(programs: &[Obs]) -> Result<(), String> {
    for o in programs {
        if let Some(c) = o.cost {
            // A `Run` past its caps is an engine reporting a clean answer for a program that was
            // in fact terminated: the consumer would ledger the console as the round's result.
            if c.ops > c.ops_cap || c.ms > c.wall_ms {
                return Err(format!(
                    "program `{}` reported a Run costing {} ops / {} ms under caps of {} ops / \
                     {} ms; a cap breach is a JsError, not output",
                    o.program, c.ops, c.ms, c.ops_cap, c.wall_ms
                ));
            }
        }
        if o.cancelled && o.ran {
            return Err(format!(
                "program `{}` was cancelled and still reported a Run",
                o.program
            ));
        }
    }
    Ok(())
}

/// The spec `JsPlugin::invariants` returns.
pub fn a_cancelled_program_never_reports_a_run() -> InvariantSpec {
    InvariantSpec {
        name: "a_cancelled_program_never_reports_a_run",
        plugin: PLUGIN,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

const PLUGIN: &str = crate::PLUGIN_NAME;

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    evaluate(&seen()).map_err(|detail| InvariantViolation {
        invariant: "a_cancelled_program_never_reports_a_run",
        plugin: PLUGIN,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(ran: bool, cancelled: bool, cost: Option<Cost>) -> Obs {
        Obs {
            fiber: FiberUid(1),
            program: "deadbeef".into(),
            ran,
            errored: !ran,
            cancelled,
            cost,
        }
    }

    fn cost(ops: u64, ms: u64) -> Option<Cost> {
        Some(Cost {
            ops,
            ms,
            ops_cap: 1_000,
            wall_ms: 100,
        })
    }

    #[test]
    fn an_uncancelled_outcome_of_either_kind_is_clean() {
        assert_eq!(evaluate(&[obs(true, false, cost(10, 5))]), Ok(()));
        assert_eq!(evaluate(&[obs(false, false, None)]), Ok(()));
        assert_eq!(evaluate(&[]), Ok(()), "an idle session is vacuously clean");
    }

    #[test]
    fn a_cancelled_error_is_clean_a_cancelled_run_is_not() {
        assert_eq!(evaluate(&[obs(false, true, None)]), Ok(()));
        let d =
            evaluate(&[obs(true, true, cost(10, 5))]).expect_err("cancel + Run must be reported");
        assert!(d.contains("cancelled"), "{d}");
    }

    #[test]
    fn a_run_that_claims_a_cost_past_its_caps_is_a_violation() {
        let d = evaluate(&[obs(true, false, cost(1_001, 5))]).expect_err("an ops breach");
        assert!(d.contains("1001 ops"), "{d}");
        let d = evaluate(&[obs(true, false, cost(10, 101))]).expect_err("a wall breach");
        assert!(d.contains("101 ms"), "{d}");
        // Exactly at the cap is not a breach.
        assert_eq!(evaluate(&[obs(true, false, cost(1_000, 100))]), Ok(()));
    }
}
