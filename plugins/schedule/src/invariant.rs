//! §0.2 runtime invariant for `bough-plugin-schedule`:
//!
//! **A registered job's name is unique in the tree, and every fire produces exactly one [`JobRun`]
//! in `JobInfo.last` and exactly one `schedule/fired` emit.** Checked against the Provider's own
//! `jobs()` and the recorded event stream, not documented.
//!
//! [`JobRun`]: crate::JobRun

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. WP-1.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
