//! §0.2 runtime invariant for `bough-plugin-js`:
//!
//! **Every `Program` that ends does so with EXACTLY ONE terminal outcome — a `Run` or a
//! `JsError` — and never both; a cancelled program never reports a `Run`.**
//!
//! The seam records one [`Obs`] per finished program, so an engine that both returns output and
//! reports a cap breach is reported here rather than being discovered as a program whose console
//! disagrees with its error line. WP-1 owns the recorder and the wiring.

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
        if o.ran && o.errored {
            return Err(format!(
                "program `{}` reported BOTH a Run and a JsError; a program has exactly one \
                 terminal outcome",
                o.program
            ));
        }
        if !o.ran && !o.errored {
            return Err(format!(
                "program `{}` ended with NO terminal outcome; a caller would wait forever",
                o.program
            ));
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
pub fn exactly_one_terminal_outcome() -> InvariantSpec {
    InvariantSpec {
        name: "every_program_ends_with_exactly_one_terminal_outcome",
        plugin: PLUGIN,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

const PLUGIN: &str = crate::PLUGIN_NAME;

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    evaluate(&seen()).map_err(|detail| InvariantViolation {
        invariant: "every_program_ends_with_exactly_one_terminal_outcome",
        plugin: PLUGIN,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(ran: bool, errored: bool, cancelled: bool) -> Obs {
        Obs {
            fiber: FiberUid(1),
            program: "deadbeef".into(),
            ran,
            errored,
            cancelled,
        }
    }

    #[test]
    fn one_outcome_is_clean() {
        assert_eq!(evaluate(&[obs(true, false, false)]), Ok(()));
        assert_eq!(evaluate(&[obs(false, true, false)]), Ok(()));
        assert_eq!(evaluate(&[]), Ok(()), "an idle session is vacuously clean");
    }

    #[test]
    fn both_outcomes_is_a_violation() {
        let d = evaluate(&[obs(true, true, false)]).expect_err("both must be reported");
        assert!(d.contains("BOTH"), "{d}");
    }

    #[test]
    fn no_outcome_is_a_violation() {
        let d = evaluate(&[obs(false, false, false)]).expect_err("none must be reported");
        assert!(d.contains("NO terminal outcome"), "{d}");
    }

    #[test]
    fn a_cancelled_program_never_reports_a_run() {
        let d = evaluate(&[obs(true, false, true)]).expect_err("cancel + Run must be reported");
        assert!(d.contains("cancelled"), "{d}");
    }
}
