//! Subagents (port of `src/agents/`): spawn caps and leases, launch/result
//! building, and the wake-note pipeline. Never references the server crate.

pub mod caps;
pub mod notes;
pub mod subagent;
#[cfg(test)]
pub(crate) mod testkit;

use crate::types::{Db, SharedDb};

/// Run one closure under the db lock, poison-proof: a panic in an earlier
/// holder must not wedge every later launch.
pub(crate) fn with_db<T>(db: &SharedDb, f: impl FnOnce(&dyn Db) -> T) -> T {
    let guard = db.lock().unwrap_or_else(|p| p.into_inner());
    f(&*guard)
}
