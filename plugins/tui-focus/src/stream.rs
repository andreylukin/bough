//! Invariant: the tee REPLACES NOTHING and SHORT-CIRCUITS NOTHING. It calls `next(value)` so the
//! adapter fills the slot, then takes the stream and puts back a wrapper that appends every
//! `Chunk::TextDelta` into [`LiveText`]; with no ambient initiator it delegates untouched.
//! Attribution comes from `bough_plugin_agents::initiator::current()` (§2): ambient, never
//! authorization.

use bough_plugin_agents::AgentId;

/// The live tail that has streamed but not yet flushed to `thought/text`.
#[derive(Clone, Debug, Default)]
pub struct LiveText {
    pub agent: Option<AgentId>,
    pub text: String,
}

/// PURE, and the rule that makes streaming flicker-free (P3-D12): the durable `thought/text`
/// steps of a step index concatenate to a prefix of what streamed, so the trailing step renders
/// `live` whenever `live.len() >= durable.len()`, and the durable text otherwise.
pub fn trailing_text<'a>(_durable: &'a str, _live: &'a str) -> &'a str {
    todo!("WP-4")
}
