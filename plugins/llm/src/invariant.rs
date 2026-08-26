//! §0.2 runtime invariant for `bough-plugin-llm`:
//!
//! **Every `llm/stream` stream ends with exactly ONE terminal chunk, and nothing follows it.**
//!
//! The seam wraps every stream it hands out and records the chunk shape it saw, so a provider
//! that yields two `End`s, an `End` after a `Failed`, or nothing at all is reported here rather
//! than being discovered as a hung wake. WP-1 owns the recorder and the check.

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};

/// One observed stream, as the seam's wrapper saw it.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    /// The request digest, so a violation names the request it belongs to.
    pub request: String,
    /// How many terminal chunks the stream carried.
    pub terminals: u32,
    /// How many chunks arrived AFTER the first terminal one.
    pub after_terminal: u32,
}

/// Record one finished stream. Called by the seam's wrapper. WP-1.
pub fn record(_obs: Obs) {
    todo!("WP-1: push onto the recorded stream")
}

/// Forget everything recorded for `fiber` (registered as an inverse by `apply`). WP-1.
pub fn forget(_fiber: FiberUid) {
    todo!("WP-1: drop this fiber's observations so a reload starts clean")
}

/// Everything recorded so far, oldest first. WP-1.
pub fn seen() -> Vec<Obs> {
    todo!("WP-1: read the recorded stream")
}

/// The whole invariant as a pure function of the observed stream. WP-1.
pub fn evaluate(_stream: &[Obs]) -> Result<(), String> {
    todo!("WP-1: exactly one terminal chunk, nothing after it")
}

/// The spec `LlmPlugin::invariants` returns.
pub fn every_stream_ends_once() -> InvariantSpec {
    InvariantSpec {
        name: "every_stream_ends_with_exactly_one_terminal_chunk",
        plugin: PLUGIN,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

const PLUGIN: &str = crate::PLUGIN_NAME;

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    evaluate(&seen()).map_err(|detail| InvariantViolation {
        invariant: "every_stream_ends_with_exactly_one_terminal_chunk",
        plugin: PLUGIN,
        entry: ctx.entry_id().clone(),
        detail,
    })
}
