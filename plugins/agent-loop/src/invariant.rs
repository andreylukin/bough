//! §0.2 runtime invariants for `bough-plugin-agent-loop`:
//!
//! 1. **The request the adapter was handed RECONSTRUCTS from the ledger, byte for byte.** This is
//!    §0.2's "model-visible ⟺ ledgered" made checkable: the loop records each request it sent
//!    (bounded, last N wakes) and the check rebuilds it from the wake's own steps and compares.
//!    A side-channel message — anything that reached the model without a step — is exactly what
//!    it catches.
//! 2. **Unconsumed ordinary mail at any `wake_end` implies a scheduled drain wake** (§5's
//!    standing invariant).
//! 3. **Every `wake/start` has a `wake/end`, or is the live one.**
//!
//! P2-D18: the reconstruction evaluator is a PURE FUNCTION here, imported by
//! `agent-loop-scripted` for its own invariant. Two recorders, one evaluator, so the copies
//! cannot drift.

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{Step, WakeId};
use bough_plugin_llm::LlmRequest;

/// One request as it was actually handed to an adapter.
#[derive(Clone, Debug)]
pub struct SentRequest {
    pub fiber: FiberUid,
    pub wake: WakeId,
    pub step_index: u32,
    pub request: LlmRequest,
}

/// Record one sent request. Called by the loop, and by `agent-loop-scripted`. WP-4.
pub fn record(_sent: SentRequest) {
    todo!("WP-4: push onto the bounded recorded stream")
}

/// Forget everything recorded for `fiber`. WP-4.
pub fn forget(_fiber: FiberUid) {
    todo!("WP-4")
}

/// Everything recorded so far, oldest first. WP-4.
pub fn seen() -> Vec<SentRequest> {
    todo!("WP-4")
}

/// THE shared evaluator (P2-D18): rebuild each recorded request from the wake's steps and compare
/// digests. Pure over (recorded requests, steps), so both loop providers check the same thing.
///
/// WP-4.
pub fn evaluate_reconstruction(_sent: &[SentRequest], _steps: &[Step]) -> Result<(), String> {
    todo!("WP-4: reconstruct via transcript::rebuild + the request/header body, compare digests")
}

/// The standing mail invariant, as a pure function. WP-4.
pub fn evaluate_mail(_steps: &[Step], _drain_scheduled: bool) -> Result<(), String> {
    todo!("WP-4: unconsumed ordinary mail => a drain wake is scheduled")
}

/// Every `wake/start` closed, or is the live one. WP-4.
pub fn evaluate_wake_pairing(_steps: &[Step], _live: Option<&WakeId>) -> Result<(), String> {
    todo!("WP-4")
}

/// The specs `AgentLoopPlugin::invariants` returns.
pub fn specs() -> Vec<InvariantSpec> {
    vec![
        InvariantSpec {
            name: "every_request_reconstructs_from_the_ledger",
            plugin: crate::PLUGIN_NAME,
            cadence: Cadence::OnQuiesce,
            check: |ctx| Box::pin(check_reconstruction(ctx)),
        },
        InvariantSpec {
            name: "unconsumed_ordinary_mail_implies_a_scheduled_drain_wake",
            plugin: crate::PLUGIN_NAME,
            cadence: Cadence::OnQuiesce,
            check: |ctx| Box::pin(check_mail(ctx)),
        },
    ]
}

async fn check_reconstruction(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-4: read the ledger and evaluate the recorded requests")
}

async fn check_mail(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-4: read the ledger and evaluate the standing mail invariant")
}
