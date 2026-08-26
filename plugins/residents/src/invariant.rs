//! §0.2 runtime invariant for `bough-plugin-residents`:
//!
//! **At most one catch-up wake per agent per activation.** A second catch-up would re-read mail
//! the first already consumed and put the same evidence in front of a model twice; the check is a
//! fold over this row's own observed `request_wake` stream, per fiber.
//!
//! WP-7 owns the recorder and the check.

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::AgentName;

/// One observed catch-up request.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    pub agent: AgentName,
    pub started: bool,
}

/// Record one moment. WP-7 calls this from the catch-up pass.
pub fn record(_obs: Obs) {
    todo!("WP-7")
}

/// Drop the recorded stream. Test setup only.
pub fn clear() {
    todo!("WP-7")
}

/// PURE: the fold the check runs.
pub fn check_stream(_seen: &[Obs]) -> Result<(), String> {
    todo!("WP-7")
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "at_most_one_catch_up_wake_per_agent_per_activation",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-7")
}
