//! No runtime invariant: `tui-probe` is a TEST INSTRUMENT that owns no authoritative event stream
//! and no data relation (§0.2). What it proves is proved by the tests and scripts that mount it —
//! `scripts/tui/08-restore.sh` and `crates/bough/tests/tui_boot.rs` — not by a check of its own.

use bough_kernel::InvariantSpec;

/// None, deliberately. See the module comment.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
