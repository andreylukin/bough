//! §0.2 runtime invariant for `bough-plugin-schedule-cron`:
//!
//! **Every fire this Provider performs writes exactly one row into its own last-run table and emits exactly one `schedule/fired`, and no job runs longer than `job_timeout_ms`.**

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
