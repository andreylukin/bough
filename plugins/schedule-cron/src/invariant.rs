//! §0.2 runtime invariant for `bough-plugin-schedule-cron`:
//!
//! **Every fire this Provider performs writes exactly one row into its own last-run table, and
//! that row IS the listing's `last`.** The timeout half of the bound is structural rather than
//! observable after the fact — `run_one` abandons the task at `job_timeout_ms` — so what is
//! checked here is the half a stale or lost write would break: a job whose listing says it ran
//! and whose store does not (which is what `catch_up: true` would then get wrong after a
//! restart).

use std::collections::BTreeMap;

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_schedule::{JobInfo, JobRun};
use parking_lot::Mutex;

use crate::CronScheduler;

/// The live Provider, if this process has one. Set by `apply`, cleared by its disposer.
static LIVE: Mutex<Option<std::sync::Weak<CronScheduler>>> = Mutex::new(None);

/// Publish the live scheduler for the invariant to read.
pub fn publish(me: &std::sync::Arc<CronScheduler>) {
    *LIVE.lock() = Some(std::sync::Arc::downgrade(me));
}

/// Withdraw it.
pub fn withdraw() {
    *LIVE.lock() = None;
}

/// PURE: the whole check.
pub fn evaluate(jobs: &[JobInfo], stored: &BTreeMap<String, JobRun>) -> Result<(), String> {
    for job in jobs {
        let Some(last) = &job.last else { continue };
        match stored.get(job.name.as_str()) {
            None => {
                return Err(format!(
                    "`{}` reports a run at {} that its own last-run table does not hold: \
                     `catch_up` would refire it after a restart",
                    job.name, last.at
                ))
            }
            Some(row) if row.at != last.at => {
                return Err(format!(
                    "`{}` reports a run at {} but its last-run table holds {}",
                    job.name, last.at, row.at
                ))
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "every_fire_is_persisted_in_the_rows_own_last_run_table",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(check(ctx)),
    }]
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    let fail = |detail: String| InvariantViolation {
        invariant: "every_fire_is_persisted_in_the_rows_own_last_run_table",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    let live = LIVE.lock().clone();
    let Some(me) = live.and_then(|w| w.upgrade()) else {
        // The row is gone: there is nothing to state about a scheduler that has left.
        return Ok(());
    };
    let stored = me.stored().map_err(|e| fail(e.to_string()))?;
    use bough_plugin_schedule::Scheduler as _;
    evaluate(&me.jobs(), &stored).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_kernel::EntryId;
    use bough_plugin_schedule::{Cadence as JobCadence, FireReason, JobName, JobOutcome};

    fn at(secs: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("a fixed instant")
    }

    fn run(secs: i64) -> JobRun {
        JobRun {
            at: at(secs),
            reason: FireReason::Cadence,
            outcome: JobOutcome::Ran {
                detail: "ok".into(),
            },
        }
    }

    fn info(last: Option<JobRun>) -> JobInfo {
        JobInfo {
            name: JobName::new("sweep"),
            cadence: JobCadence::Every { every_ms: 1000 },
            owner: EntryId::new("row"),
            next: None,
            last,
        }
    }

    #[test]
    fn a_reported_run_that_was_never_persisted_is_a_violation() {
        let err = evaluate(&[info(Some(run(1)))], &BTreeMap::new()).unwrap_err();
        assert!(err.contains("does not hold"), "{err}");
    }

    #[test]
    fn a_persisted_run_at_another_time_is_a_violation() {
        let stored = BTreeMap::from([("sweep".to_string(), run(2))]);
        let err = evaluate(&[info(Some(run(1)))], &stored).unwrap_err();
        assert!(err.contains("last-run table holds"), "{err}");
    }

    #[test]
    fn a_job_that_never_ran_is_not_a_violation() {
        evaluate(&[info(None)], &BTreeMap::new()).expect("nothing has fired yet");
    }

    #[test]
    fn a_reported_run_that_matches_its_stored_row_holds() {
        let stored = BTreeMap::from([("sweep".to_string(), run(1))]);
        evaluate(&[info(Some(run(1)))], &stored).expect("one fire, one persisted row");
    }
}
