//! §0.2 runtime invariant for `bough-plugin-agents`:
//!
//! **Status never repeats; a disposed agent is terminal (no status change and no wake after
//! disposal); and at most one factory is ever set.**
//!
//! The check is a fold over the observed `agent/status` + `agent/disposed` + `agent/wake`
//! streams, per fiber and bounded — Phase 1's lesson: two fibers are two streams, and a reload
//! must not read as a violation of its own predecessor. WP-2 owns the recorder and the check.

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};

use crate::agent::Status;
use crate::ids::AgentId;

/// One observed moment of an agent's life.
#[derive(Clone, Debug, PartialEq)]
pub enum Obs {
    Status {
        fiber: FiberUid,
        agent: AgentId,
        from: Status,
        to: Status,
    },
    Disposed {
        fiber: FiberUid,
        agent: AgentId,
    },
    WakeStarted {
        fiber: FiberUid,
        agent: AgentId,
    },
}

/// Record one moment. Called by the listeners `AgentsPlugin::apply` registers. WP-2.
pub fn record(_obs: Obs) {
    todo!("WP-2: push onto the recorded stream")
}

/// Forget everything recorded for `fiber`, as an inverse of `apply`. WP-2.
pub fn forget(_fiber: FiberUid) {
    todo!("WP-2: drop this fiber's observations")
}

/// Everything recorded so far, oldest first. WP-2.
pub fn seen() -> Vec<Obs> {
    todo!("WP-2: read the recorded stream")
}

/// Drop the recorded stream. Test setup only. WP-2.
pub fn clear() {
    todo!("WP-2")
}

/// The whole invariant as a pure function of the observed stream: the first violation wins, and
/// the detail names the agent and what it did.
///
/// WP-2.
pub fn evaluate(_stream: &[Obs]) -> Result<(), String> {
    todo!("WP-2: no repeat, nothing after disposal")
}

/// The spec `AgentsPlugin::invariants` returns.
pub fn agent_lifecycle_is_sane() -> InvariantSpec {
    InvariantSpec {
        name: "agent_status_never_repeats_and_disposal_is_terminal",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    evaluate(&seen()).map_err(|detail| InvariantViolation {
        invariant: "agent_status_never_repeats_and_disposal_is_terminal",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}
