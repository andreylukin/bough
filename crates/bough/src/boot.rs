//! Invariant: teardown before exit (§0.1 item 2). Every exit path — a failed activation assertion,
//! SIGINT, a `--check` run — awaits `kernel.shutdown()` before returning, so a Phase-3 TUI failure
//! still restores the terminal.
//!
//! And: an enabled row that never activates is a BOOT FAILURE (§0.2, Decision D12). At boot it is
//! fatal and names every unresolved row with its unmet keys; during a live recompose it is a
//! `kernel/rows-unresolved` warning and the tree stays.

use std::process::ExitCode;

use bough_kernel::TreeSnapshot;

use crate::cli::{BootError, Cli};

/// Compose, mount, quiesce, assert, then either run or exit.
pub async fn boot(cli: Cli) -> Result<ExitCode, BootError> {
    todo!("WP-5")
}

/// After quiesce, every row with `disabled == false` must be ACTIVE.
///
/// On failure the caller prints each unresolved row with its unmet keys, awaits
/// `kernel.shutdown()`, and exits 1.
pub fn assert_all_activated(s: &TreeSnapshot) -> Result<(), BootError> {
    todo!("WP-5")
}

/// Render the unresolved rows for the boot-failure message: one line per row, naming the row, its
/// plugin and each unmet key.
pub fn describe_unresolved(s: &TreeSnapshot) -> String {
    todo!("WP-5")
}
