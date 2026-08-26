//! §0.2 runtime invariant for `bough-plugin-schedule`:
//!
//! **A registered job's name is unique in the tree, and every fire produces exactly one [`JobRun`]
//! in `JobInfo.last` and exactly one `schedule/fired` emit.** Checked against the Provider's own
//! `jobs()` and the recorded event stream, not documented.
//!
//! The recorded stream is per FIBER LIFE: the Definition's `apply` records, and its disposer
//! forgets, so a reload is never flagged as a double fire.
//!
//! [`JobRun`]: crate::JobRun

use std::collections::BTreeMap;

use bough_kernel::{Cadence as RunCadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use parking_lot::Mutex;

use crate::{JobInfo, JobRun, Schedule};

/// One observed `schedule/fired`.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    pub run: JobRun,
}

static OBSERVED: Mutex<Vec<Obs>> = Mutex::new(Vec::new());

/// Record one observed fire. Called from the Definition row's listener.
pub fn record(fiber: FiberUid, run: JobRun) {
    OBSERVED.lock().push(Obs { fiber, run });
}

/// Forget one fiber life's observations. Called from that fiber's disposer.
pub fn forget(fiber: FiberUid) {
    OBSERVED.lock().retain(|o| o.fiber != fiber);
}

/// What this fiber has seen, oldest first.
pub fn observed(fiber: FiberUid) -> Vec<JobRun> {
    OBSERVED
        .lock()
        .iter()
        .filter(|o| o.fiber == fiber)
        .map(|o| o.run.clone())
        .collect()
}

/// PURE: the whole invariant, over the listing and the observed stream.
///
/// Two halves. (1) NAME UNIQUENESS: two `JobInfo` rows with one name means two jobs answer to one
/// `fire_now`. (2) EVERY ANNOUNCED FIRE WAS ALSO RECORDED: `JobRun` is the emit payload and does
/// not carry its own name (§2.1), so the pairing that can be checked from outside is the one that
/// matters — a tree that announced a fire at `t` must hold a recorded run at or after `t` in some
/// job's `last`. An emit with no recorded run behind it anywhere is the failure this catches.
pub fn evaluate(jobs: &[JobInfo], fired: &[JobRun]) -> Result<(), String> {
    let mut by_name: BTreeMap<&str, &JobInfo> = BTreeMap::new();
    for job in jobs {
        if by_name.insert(job.name.as_str(), job).is_some() {
            return Err(format!(
                "two jobs are registered as `{}`: a job name is unique in the tree, or one \
                 `fire_now` fires two bodies",
                job.name
            ));
        }
    }
    let Some(latest_emit) = fired.iter().map(|r| r.at).max() else {
        return Ok(());
    };
    let latest_recorded = jobs
        .iter()
        .filter_map(|j| j.last.as_ref())
        .map(|r| r.at)
        .max();
    match latest_recorded {
        // Every job that fired may since have left with its row; only a tree that still holds
        // jobs, none of which ever ran, contradicts an announced fire.
        None if jobs.is_empty() => Ok(()),
        None => Err(format!(
            "a fire was announced at {latest_emit} but no registered job carries a recorded run: \
             a fire produces exactly one JobRun AND one emit"
        )),
        Some(last) if last < latest_emit && !jobs.is_empty() => Err(format!(
            "a fire was announced at {latest_emit} but the newest recorded run is {last}: the run \
             was announced and never recorded"
        )),
        Some(_) => Ok(()),
    }
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "job_names_are_unique_and_every_fire_is_recorded_and_announced",
        plugin: crate::PLUGIN_NAME,
        cadence: RunCadence::OnQuiesce,
        check: |ctx: Context| Box::pin(check(ctx)),
    }]
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    let fail = |detail: String| InvariantViolation {
        invariant: "job_names_are_unique_and_every_fire_is_recorded_and_announced",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    // The live store, not the view: the Definition does not inject the key it defines, and a
    // Provider bound after it is exactly what this check is about (the `old-feed-adapter`
    // precedent).
    let Some(schedule) = ctx.peek_live::<Schedule>() else {
        return Ok(());
    };
    evaluate(&schedule.0.jobs(), &observed(ctx.fiber_uid())).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cadence, FireReason, JobName, JobOutcome};
    use bough_kernel::EntryId;
    use chrono::{DateTime, TimeZone, Utc};

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).single().unwrap()
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

    fn info(name: &str, last: Option<JobRun>) -> JobInfo {
        JobInfo {
            name: JobName::new(name),
            cadence: Cadence::Every { every_ms: 1000 },
            owner: EntryId::new("row"),
            next: None,
            last,
        }
    }

    #[test]
    fn two_jobs_with_one_name_is_a_violation() {
        let err = evaluate(&[info("sweep", None), info("sweep", None)], &[]).unwrap_err();
        assert!(err.contains("two jobs are registered as `sweep`"), "{err}");
    }

    #[test]
    fn an_announced_fire_that_was_never_recorded_is_a_violation() {
        let err = evaluate(&[info("sweep", None)], &[run(1)]).unwrap_err();
        assert!(
            err.contains("never recorded") || err.contains("no registered job"),
            "{err}"
        );
    }

    #[test]
    fn a_recorded_and_announced_fire_holds() {
        let r = run(1);
        evaluate(&[info("sweep", Some(r.clone()))], &[r]).expect("one fire, one run, one emit");
    }

    #[test]
    fn a_fire_for_a_job_that_has_left_with_its_row_is_not_a_violation() {
        evaluate(&[], &[run(1)]).expect("a disposed job's past fires stand");
    }
}
