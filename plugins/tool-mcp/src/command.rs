//! Invariant: `/mcp call` validates its JSON against the TOOL'S OWN input schema before the call,
//! so a malformed argument is `CommandError::BadArgs` naming the usage and never a foreign server's
//! error message. The output cites the call's cite, like any other pull (§6).

use bough_kernel::{Context, EffectHandle, PluginError};
use bough_plugin_commands::CommandsHandle;
use bough_plugin_mcp::McpHandle;

/// `/mcp call <server> <tool> <json>` and `/mcp list [server]`. WP-5.
pub async fn register(
    ctx: &Context,
    commands: &CommandsHandle,
    mcp: &McpHandle,
) -> Result<EffectHandle, PluginError> {
    let _ = (ctx, commands, mcp);
    todo!("WP-5")
}

/// The usage line a bad invocation quotes.
pub const USAGE: &str = "/mcp call <server> <tool> <json> | /mcp list [server]";
