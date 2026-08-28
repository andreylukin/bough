//! §0.2 runtime invariant for `bough-plugin-mcp-subprocess`:
//!
//! **A process's crash and restart never removes its registration on `ctx.mcp`.** The registration
//! is an effect of the CHILD FIBER, not of the OS process, so a supervised restart must leave the
//! server set exactly as it was — that is what keeps a resident plugin's tools on `ctx.tools` while
//! its process is down, instead of a tool vanishing mid-wake.
//!
//! Checked per child: the server this fiber registered is still in `McpHandle::servers()`, whatever
//! its process is doing. The name is the child entry's last segment, which is exactly how
//! [`crate::child_entry`] mints it.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_mcp::{Mcp, ServerName};

const NAME: &str = "a_restart_never_withdraws_the_server_registration";

/// PURE: the server name a child entry id carries.
pub fn server_of(entry_id: &str) -> &str {
    entry_id.rsplit('.').next().unwrap_or(entry_id)
}

/// PURE: the whole check.
pub fn evaluate(server: &str, registered: &[ServerName]) -> Result<(), String> {
    if registered.iter().any(|s| s.as_str() == server) {
        return Ok(());
    }
    Err(format!(
        "server `{server}` is not registered on `ctx.mcp` while its child fiber is ACTIVE; a \
         crash must not withdraw the registration, or its tools vanish mid-wake"
    ))
}

/// The specs the CHILD row contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: NAME,
        plugin: crate::PROCESS_PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }]
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    let Some(mcp) = ctx.peek_live::<Mcp>() else {
        // The row is being torn down: there is nothing to state about a seam that is gone.
        return Ok(());
    };
    let server = server_of(ctx.entry_id().as_str()).to_string();
    evaluate(&server, &mcp.servers()).map_err(|detail| InvariantViolation {
        invariant: NAME,
        plugin: crate::PROCESS_PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_name_is_the_child_entrys_last_segment() {
        assert_eq!(server_of("mcp.subprocess.echo"), "echo");
        assert_eq!(server_of("echo"), "echo");
    }

    #[test]
    fn a_registered_server_passes_and_a_withdrawn_one_does_not() {
        let registered = vec![ServerName::new("echo"), ServerName::new("other")];
        assert!(evaluate("echo", &registered).is_ok());
        let err = evaluate("gone", &registered).expect_err("violation");
        assert!(err.contains("vanish mid-wake"), "{err}");
    }
}
