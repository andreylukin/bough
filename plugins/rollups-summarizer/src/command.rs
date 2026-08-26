//! Invariant (P3-D8): `/seal` runs a governance pass and RENDERS the report; it dispatches no
//! model turn on the agent's behalf and appends nothing but the pass's own steps. `--plan` writes
//! nothing at all.

use bough_kernel::{Context, PluginError};

use crate::RecapSummarizer;

/// Register `/seal`, if a `commands` registry is bound (P4-D8).
///
/// ```text
/// /seal [agent] [--plan]      run (or, with --plan, only report) a seal pass for the agent
/// ```
pub async fn register(_ctx: &Context, _summarizer: &RecapSummarizer) -> Result<(), PluginError> {
    todo!("WP-2: register /seal when `commands` is present")
}
