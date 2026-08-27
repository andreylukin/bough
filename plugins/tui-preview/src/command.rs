//! Invariant: the command FOCUSES the pane and takes a fresh snapshot; it never registers a second
//! way to assemble a projection (D-C3 — the pane owns no write path and no second implementation).

use bough_kernel::{Context, PluginError};

/// The command this row registers: `/preview [agent]`.
pub const NAME: &str = "preview";

/// The plain-language summary `/help` lists it under (phase ux1 §2.8).
pub const SUMMARY: &str = "show the exact context this agent would wake with";

/// Register `/preview`, if a `commands` registry is bound.
///
/// ABSENT is headless: the row works with no command surface. An ERROR is the kernel refusing the
/// read and is a boot failure, never a row that silently registered nothing (§0.2).
///
/// WP-1.
pub async fn register(ctx: &Context) -> Result<(), PluginError> {
    let _ = ctx;
    todo!("WP-1: register /preview against the optional commands key")
}
