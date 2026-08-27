//! Invariant: the command FOCUSES the pane and sets its filter; it registers no second way to read
//! the ledger (D-C3 — the pane owns no write path and no query of its own).

use bough_kernel::{Context, PluginError};

/// The command this row registers: `/timeline [filter…]`.
pub const NAME: &str = "timeline";

/// The plain-language summary `/help` lists it under (phase ux1 §2.8).
pub const SUMMARY: &str = "show what every agent did, newest last, with filters";

/// Register `/timeline`, if a `commands` registry is bound.
///
/// ABSENT is headless: the row works with no command surface. An ERROR is the kernel refusing the
/// read and is a boot failure, never a row that silently registered nothing (§0.2).
///
/// WP-2.
pub async fn register(ctx: &Context) -> Result<(), PluginError> {
    let _ = ctx;
    todo!("WP-2: register /timeline against the optional commands key")
}
