//! Invariant (P3-D8): `/reconsolidate` runs a pass and RENDERS the report; `--plan` writes
//! nothing at all.

use bough_kernel::{Context, PluginError};

use crate::ReconHandle;

/// Register `/reconsolidate`, if a `commands` registry is bound.
///
/// ```text
/// /reconsolidate [agent] [--plan] [--since <seq>]
/// ```
pub async fn register(_ctx: &Context, _recon: &ReconHandle) -> Result<(), PluginError> {
    todo!("WP-3: register /reconsolidate when `commands` is present")
}
