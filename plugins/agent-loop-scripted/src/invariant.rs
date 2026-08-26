//! §0.2 runtime invariant for `bough-plugin-agent-loop-scripted`:
//!
//! **The same request-reconstruction check `agent-loop` holds** — imported, not copied (P2-D18):
//! `bough_plugin_agent_loop::invariant::evaluate_reconstruction` is the one evaluator, and this
//! row is its second recorder. Copying it would let the copies drift, and the whole point of the
//! swap gate is that both providers are held to the SAME ledger protocol.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};

/// The spec `ScriptedLoopPlugin::invariants` returns.
pub fn requests_reconstruct_from_the_ledger() -> InvariantSpec {
    InvariantSpec {
        name: "every_request_reconstructs_from_the_ledger",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-5: read the ledger and call agent_loop::invariant::evaluate_reconstruction")
}
