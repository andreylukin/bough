//! Invariant (§16): the keyboard path and the click path decide through the SAME seam. `/claims`,
//! `/accept`, `/edit` and `/reject` call [`crate::ClaimsHandle::decide`] with [`crate::Actor::Andrey`]
//! exactly as a click on a claim card does, so the two surfaces cannot drift apart.

use bough_kernel::{Context, PluginError};

use crate::ClaimsHandle;

/// Register `/claims`, `/accept <claim>`, `/edit <claim> <text…>` and `/reject <claim> <reason…>`,
/// if `commands` is bound.
pub async fn register(_ctx: &Context, _claims: &ClaimsHandle) -> Result<(), PluginError> {
    todo!("WP-4: register the four claim commands when `commands` is bound")
}
