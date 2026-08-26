//! Invariant: this row's last-run table is ITS OWN sqlite file and never the ledger. A job firing
//! is not model-visible, so it is not a step (§0.2); and `catch_up` needs the value to survive a
//! restart, so it may not live in memory either.

use std::path::Path;

use bough_plugin_schedule::{JobName, JobRun, ScheduleError};

/// Per-job last run, in the row's own sqlite file.
pub struct RunStore {
    // rusqlite::Connection. Never held across an await: it is `Send` but not `Sync`.
}

impl RunStore {
    /// Open (creating the schema if absent). WP-1.
    pub fn open(path: &Path) -> Result<RunStore, ScheduleError> {
        let _ = path;
        todo!("WP-1")
    }
    /// The last recorded run of a job, if any. WP-1.
    pub fn get(&self, name: &JobName) -> Result<Option<JobRun>, ScheduleError> {
        let _ = name;
        todo!("WP-1")
    }
    /// Record a run, replacing the previous one. WP-1.
    pub fn set(&self, name: &JobName, run: &JobRun) -> Result<(), ScheduleError> {
        let _ = (name, run);
        todo!("WP-1")
    }
}
