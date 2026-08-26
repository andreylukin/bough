//! Invariant (§16): dormancy is REACHABLE from the surface. `/sleep`, `/wake` and `/dormant` are
//! registered only when a `commands` registry is bound — headless binds none and the row activates
//! anyway (the P4-D8 precedent).

use bough_kernel::{Context, PluginError};

use crate::DormancyHandle;

/// Register `/sleep <agent> [reason]`, `/wake <agent>` and `/dormant`, if `commands` is bound.
pub async fn register(_ctx: &Context, _dormancy: &DormancyHandle) -> Result<(), PluginError> {
    todo!("WP-2: register /sleep, /wake and /dormant when `commands` is bound")
}
