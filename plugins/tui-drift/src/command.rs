//! Invariant (D-C10): this row registers `/driftboard`, NOT `/drift`. `drift-watch` already owns
//! `/drift`, and a pane does not shadow a registered command. The dashboard's reset is
//! `drift-watch`'s `/reset`, reached through `PaneOutcome::Command` (D-C3).

use bough_kernel::{Context, PluginError};

/// The command this row registers: `/driftboard [agent]`.
pub const NAME: &str = "driftboard";

/// The plain-language summary `/help` lists it under (phase ux1 §2.8).
pub const SUMMARY: &str = "show how steady every agent has been lately";

/// Register `/driftboard`, if a `commands` registry is bound.
///
/// ABSENT is headless: the row works with no command surface. An ERROR is the kernel refusing the
/// read and is a boot failure, never a row that silently registered nothing (§0.2).
///
/// WP-3.
pub async fn register(ctx: &Context) -> Result<(), PluginError> {
    let _ = ctx;
    todo!("WP-3: register /driftboard against the optional commands key")
}
