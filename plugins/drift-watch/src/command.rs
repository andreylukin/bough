//! Invariant (P3-D8): these three commands report, reset or supersede — none of them dispatches a
//! model turn on the agent's behalf. `/supersede` is a thin call to `ctx.rollups.supersede`: it
//! lives here, not on the summarizer, because §8 puts "if a tier block itself is suspected bad"
//! inside the drift-watch paragraph and the suspicion is what drift-watch surfaces.

use bough_kernel::{Context, PluginError};

use crate::DriftHandle;

/// Register `/drift`, `/reset` and `/supersede`, if a `commands` registry is bound.
///
/// ```text
/// /drift [agent]                      render the signals and any flags
/// /reset <agent>                      §8's one-command reset
/// /supersede <rollup-id> <reason>     supersede a suspected-bad tier block
/// ```
pub async fn register(_ctx: &Context, _drift: &DriftHandle) -> Result<(), PluginError> {
    todo!("WP-4: register /drift, /reset and /supersede when `commands` is present")
}
