//! §0.2 runtime invariant for `bough-plugin-system-schedules`:
//!
//! **A `Pending` reconsolidation fire never fails its row, and the catch-up pass requests at most
//! one wake per live agent per fire.** The first half is P6-D2 in checkable form: the row is
//! ACTIVE and its job says `Pending`, so a `Failed` carrying "no such command" would mean the
//! decision was quietly reversed. The second is counted by the pass itself: it records what it
//! asked and how many agents it considered.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_schedule::JobOutcome;
use parking_lot::Mutex;

/// One catch-up fire, as the pass saw it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sweep {
    /// Agents of a configured kind that were not disposed.
    pub eligible: usize,
    /// `request_wake` calls made.
    pub asked: usize,
}

static SWEEPS: Mutex<Vec<Sweep>> = Mutex::new(Vec::new());

/// Record one catch-up fire.
pub fn record(sweep: Sweep) {
    SWEEPS.lock().push(sweep);
}

/// Forget everything recorded. Called by the row's disposer and by tests.
pub fn forget() {
    SWEEPS.lock().clear();
}

/// What has been recorded.
pub fn sweeps() -> Vec<Sweep> {
    SWEEPS.lock().clone()
}

/// PURE: one wake per eligible agent, never two.
pub fn evaluate_sweeps(sweeps: &[Sweep]) -> Result<(), String> {
    for s in sweeps {
        if s.asked > s.eligible {
            return Err(format!(
                "a catch-up fire asked {} agent(s) for a wake but only {} were eligible: the \
                 pass requests at most one wake per live agent",
                s.asked, s.eligible
            ));
        }
    }
    Ok(())
}

/// PURE: a missing command is `Pending`, never `Failed` (P6-D2).
pub fn evaluate_outcome(outcome: &JobOutcome) -> Result<(), String> {
    if let JobOutcome::Failed { error } = outcome {
        let e = error.to_lowercase();
        if e.contains("no command named") || e.contains("no commands seam") {
            return Err(format!(
                "the reconsolidation pass FAILED with `{error}`: an absent command is PENDING, \
                 and the row waits politely (P6-D2)"
            ));
        }
    }
    Ok(())
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "pending_never_fails_and_catch_up_asks_once_per_agent",
        plugin: crate::CATCH_UP_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(check(ctx)),
    }]
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    let fail = |detail: String| InvariantViolation {
        invariant: "pending_never_fails_and_catch_up_asks_once_per_agent",
        plugin: crate::CATCH_UP_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    evaluate_sweeps(&sweeps()).map_err(fail)?;
    let Some(schedule) = ctx.peek_live::<bough_plugin_schedule::Schedule>() else {
        return Ok(());
    };
    for job in schedule.0.jobs() {
        // Every command pass this plugin registers (`system:<command>`), except catch-up which
        // has its own shape.
        if !job.name.as_str().starts_with("system:") || job.name.as_str() == crate::CATCH_UP_JOB {
            continue;
        }
        if let Some(last) = &job.last {
            evaluate_outcome(&last.outcome).map_err(fail)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_wake_for_one_agent_is_a_violation() {
        let err = evaluate_sweeps(&[Sweep {
            eligible: 1,
            asked: 2,
        }])
        .unwrap_err();
        assert!(err.contains("at most one wake per live agent"), "{err}");
    }

    #[test]
    fn one_wake_each_holds_and_so_does_a_fire_that_woke_nobody() {
        evaluate_sweeps(&[
            Sweep {
                eligible: 3,
                asked: 3,
            },
            Sweep {
                eligible: 0,
                asked: 0,
            },
        ])
        .expect("one per agent");
    }

    #[test]
    fn a_missing_command_reported_as_failed_is_a_violation() {
        let err = evaluate_outcome(&JobOutcome::Failed {
            error: "no command named `reconsolidate` in this tree yet".into(),
        })
        .unwrap_err();
        assert!(err.contains("PENDING"), "{err}");
    }

    #[test]
    fn a_genuine_command_failure_is_not_a_violation() {
        evaluate_outcome(&JobOutcome::Failed {
            error: "the summarizer timed out".into(),
        })
        .expect("a real failure is a failure");
        evaluate_outcome(&JobOutcome::Pending {
            reason: "no command named `reconsolidate` in this tree yet".into(),
        })
        .expect("pending is the sanctioned answer");
    }
}
