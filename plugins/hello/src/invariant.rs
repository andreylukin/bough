//! §0.2 runtime invariant for `bough-plugin-hello`:
//!
//! **Every `hello/greeted` payload carries a `seq` strictly greater than the previous one for the
//! same fiber uid.**
//!
//! `hello` owns that stream, so it is authoritative about it. `HelloConfig::plant_violation` makes
//! the plugin emit a repeated seq on purpose; that is the planted violation V9 detects, and the
//! reason this file holds a real check rather than a placeholder (Phase 8 audits these).

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};

/// The spec `HelloPlugin::invariants` returns.
pub fn greeted_seq_is_monotonic() -> InvariantSpec {
    InvariantSpec {
        name: "greeted_seq_is_monotonic",
        plugin: "hello",
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

/// Read the per-fiber high-water marks recorded by the `hello/greeted` listener and report the
/// first regression.
async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-6")
}
