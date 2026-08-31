//! Invariant: this row's last-run table is ITS OWN sqlite file and never the ledger. A job firing
//! is not model-visible, so it is not a step (§0.2); and `catch_up` needs the value to survive a
//! restart, so it may not live in memory either.
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS job_runs (
//!   name    TEXT PRIMARY KEY,
//!   at      INTEGER NOT NULL,   -- ms since epoch
//!   reason  TEXT NOT NULL,      -- 'cadence' | 'catch-up' | 'manual'
//!   outcome TEXT NOT NULL,      -- 'ran' | 'pending' | 'failed'
//!   detail  TEXT NOT NULL
//! );
//! ```

use std::path::Path;

use bough_plugin_schedule::{FireReason, JobName, JobOutcome, JobRun, ScheduleError};
use parking_lot::Mutex;
use rusqlite::Connection;

/// Per-job last run, in the row's own sqlite file.
pub struct RunStore {
    /// A `rusqlite::Connection` is `Send` but not `Sync`, so it lives behind a lock and is never
    /// held across an await.
    conn: Mutex<Connection>,
}

fn state(e: impl std::fmt::Display) -> ScheduleError {
    ScheduleError::State(e.to_string())
}

/// PURE: the stored spelling of a reason, and back. Round-tripped by a test.
pub fn reason_str(r: FireReason) -> &'static str {
    match r {
        FireReason::Cadence => "cadence",
        FireReason::CatchUp => "catch-up",
        FireReason::Manual => "manual",
        FireReason::Retry => "retry",
    }
}

/// PURE: an unknown spelling reads as `Cadence` — a stored row from a future version must not
/// make the whole store unreadable.
pub fn reason_of(s: &str) -> FireReason {
    match s {
        "catch-up" => FireReason::CatchUp,
        "manual" => FireReason::Manual,
        "retry" => FireReason::Retry,
        _ => FireReason::Cadence,
    }
}

/// PURE: the stored spelling of an outcome.
pub fn outcome_parts(o: &JobOutcome) -> (&'static str, &str) {
    match o {
        JobOutcome::Ran { detail } => ("ran", detail.as_str()),
        JobOutcome::Pending { reason } => ("pending", reason.as_str()),
        JobOutcome::Failed { error } => ("failed", error.as_str()),
    }
}

/// PURE: and back.
pub fn outcome_of(kind: &str, detail: String) -> JobOutcome {
    match kind {
        "pending" => JobOutcome::Pending { reason: detail },
        "failed" => JobOutcome::Failed { error: detail },
        _ => JobOutcome::Ran { detail },
    }
}

impl RunStore {
    /// Open (creating the schema if absent).
    pub fn open(path: &Path) -> Result<RunStore, ScheduleError> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(state)?;
            }
        }
        let conn = Connection::open(path).map_err(state)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS job_runs (
               name    TEXT PRIMARY KEY,
               at      INTEGER NOT NULL,
               reason  TEXT NOT NULL,
               outcome TEXT NOT NULL,
               detail  TEXT NOT NULL
             );",
        )
        .map_err(state)?;
        Ok(RunStore {
            conn: Mutex::new(conn),
        })
    }

    /// The last recorded run of a job, if any.
    pub fn get(&self, name: &JobName) -> Result<Option<JobRun>, ScheduleError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT at, reason, outcome, detail FROM job_runs WHERE name = ?1")
            .map_err(state)?;
        let mut rows = stmt.query([name.as_str()]).map_err(state)?;
        let Some(row) = rows.next().map_err(state)? else {
            return Ok(None);
        };
        let at_ms: i64 = row.get(0).map_err(state)?;
        let reason: String = row.get(1).map_err(state)?;
        let outcome: String = row.get(2).map_err(state)?;
        let detail: String = row.get(3).map_err(state)?;
        let at = chrono::DateTime::from_timestamp_millis(at_ms)
            .ok_or_else(|| ScheduleError::State(format!("`{name}` has an unreadable time")))?;
        Ok(Some(JobRun {
            at,
            reason: reason_of(&reason),
            outcome: outcome_of(&outcome, detail),
        }))
    }

    /// Every recorded run, by job name. The invariant reads it.
    pub fn all(&self) -> Result<std::collections::BTreeMap<String, JobRun>, ScheduleError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT name, at, reason, outcome, detail FROM job_runs")
            .map_err(state)?;
        let mut rows = stmt.query([]).map_err(state)?;
        let mut out = std::collections::BTreeMap::new();
        while let Some(row) = rows.next().map_err(state)? {
            let name: String = row.get(0).map_err(state)?;
            let at_ms: i64 = row.get(1).map_err(state)?;
            let reason: String = row.get(2).map_err(state)?;
            let outcome: String = row.get(3).map_err(state)?;
            let detail: String = row.get(4).map_err(state)?;
            let Some(at) = chrono::DateTime::from_timestamp_millis(at_ms) else {
                continue;
            };
            out.insert(
                name,
                JobRun {
                    at,
                    reason: reason_of(&reason),
                    outcome: outcome_of(&outcome, detail),
                },
            );
        }
        Ok(out)
    }

    /// Record a run, replacing the previous one.
    pub fn set(&self, name: &JobName, run: &JobRun) -> Result<(), ScheduleError> {
        let (kind, detail) = outcome_parts(&run.outcome);
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO job_runs (name, at, reason, outcome, detail)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(name) DO UPDATE SET
               at = excluded.at, reason = excluded.reason,
               outcome = excluded.outcome, detail = excluded.detail",
            rusqlite::params![
                name.as_str(),
                run.at.timestamp_millis(),
                reason_str(run.reason),
                kind,
                detail
            ],
        )
        .map_err(state)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("a fixed instant")
    }

    #[test]
    fn a_job_never_run_reads_as_none() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = RunStore::open(&dir.path().join("schedule.db")).expect("a fresh store");
        assert_eq!(store.get(&JobName::new("sweep")).unwrap(), None);
    }

    #[test]
    fn a_last_run_survives_a_reopen_which_is_what_catch_up_needs() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("schedule.db");
        let run = JobRun {
            at: at(5),
            reason: FireReason::CatchUp,
            outcome: JobOutcome::Pending {
                reason: "no such command".into(),
            },
        };
        {
            let store = RunStore::open(&path).expect("a fresh store");
            store.set(&JobName::new("sweep"), &run).expect("recorded");
        }
        let store = RunStore::open(&path).expect("the same store");
        assert_eq!(store.get(&JobName::new("sweep")).unwrap(), Some(run));
    }

    #[test]
    fn a_second_run_replaces_the_first() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = RunStore::open(&dir.path().join("schedule.db")).expect("a fresh store");
        let name = JobName::new("sweep");
        for secs in [1, 2] {
            store
                .set(
                    &name,
                    &JobRun {
                        at: at(secs),
                        reason: FireReason::Cadence,
                        outcome: JobOutcome::Ran {
                            detail: format!("{secs}"),
                        },
                    },
                )
                .expect("recorded");
        }
        let last = store.get(&name).unwrap().expect("a run");
        assert_eq!(last.at, at(2));
    }
}
