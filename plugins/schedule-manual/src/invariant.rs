//! §0.2 runtime invariant for `bough-plugin-schedule-manual`:
//!
//! **No job ever runs without a `fire_now` / `fire_at` call, and no job of this Provider ever
//! carries a `next`.** The second half is the checkable form of the first: this Provider has no
//! clock, so a listing row that claims a next fire would mean something in it is timing, and a
//! `FireReason::Cadence` run could only have come from a timer it does not have.

use bough_kernel::{Cadence as RunCadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_schedule::{FireReason, JobInfo, Schedule};

/// PURE: the whole check.
pub fn evaluate(jobs: &[JobInfo]) -> Result<(), String> {
    for job in jobs {
        if let Some(next) = job.next {
            return Err(format!(
                "`{}` claims a next fire at {next}: this Provider has no clock",
                job.name
            ));
        }
        if let Some(last) = &job.last {
            if last.reason == FireReason::Cadence {
                return Err(format!(
                    "`{}` recorded a `Cadence` run at {}: this Provider fires only on demand",
                    job.name, last.at
                ));
            }
        }
    }
    Ok(())
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "no_job_fires_without_a_call",
        plugin: crate::PLUGIN_NAME,
        cadence: RunCadence::OnQuiesce,
        check: |ctx: Context| Box::pin(check(ctx)),
    }]
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    let fail = |detail: String| InvariantViolation {
        invariant: "no_job_fires_without_a_call",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    let Some(schedule) = ctx.peek_live::<Schedule>() else {
        return Ok(());
    };
    if schedule.0.provider() != crate::PLUGIN_NAME {
        // Another Provider is bound: its own invariant speaks for it.
        return Ok(());
    }
    evaluate(&schedule.0.jobs()).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_kernel::EntryId;
    use bough_plugin_schedule::{Cadence, JobName, JobOutcome, JobRun};

    fn info(next: Option<chrono::DateTime<chrono::Utc>>, last: Option<JobRun>) -> JobInfo {
        JobInfo {
            name: JobName::new("sweep"),
            cadence: Cadence::Every { every_ms: 1000 },
            owner: EntryId::new("row"),
            next,
            last,
        }
    }

    fn run(reason: FireReason) -> JobRun {
        JobRun {
            at: chrono::Utc::now(),
            reason,
            outcome: JobOutcome::Ran {
                detail: "ok".into(),
            },
        }
    }

    #[test]
    fn a_next_fire_is_a_violation() {
        let err = evaluate(&[info(Some(chrono::Utc::now()), None)]).unwrap_err();
        assert!(err.contains("has no clock"), "{err}");
    }

    #[test]
    fn a_cadence_run_is_a_violation() {
        let err = evaluate(&[info(None, Some(run(FireReason::Cadence)))]).unwrap_err();
        assert!(err.contains("fires only on demand"), "{err}");
    }

    #[test]
    fn a_manual_run_with_no_next_holds() {
        evaluate(&[info(None, Some(run(FireReason::Manual)))]).expect("fired on demand");
    }
}
