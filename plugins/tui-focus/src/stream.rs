//! Invariant: the tee REPLACES NOTHING and SHORT-CIRCUITS NOTHING. It calls `next(value)` so the
//! adapter fills the slot, then takes the stream and puts back a wrapper that appends every
//! `Chunk::TextDelta` into [`LiveText`]; with no ambient initiator it delegates untouched.
//! Attribution comes from `bough_plugin_agents::initiator::current()` (§2): ambient, never
//! authorization.

use std::sync::Arc;

use bough_plugin_agents::AgentId;
use bough_plugin_llm::{Chunk, StreamCall};
use futures::StreamExt;
use parking_lot::Mutex;

/// The live tail that has streamed but not yet flushed to `thought/text`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveText {
    pub agent: Option<AgentId>,
    pub text: String,
}

impl LiveText {
    /// Drop the tail. Driven by `agent/step` `Phase::Start` and `agent/wake` `Phase::End`: at both
    /// moments the durable steps are the whole truth and anything still held here would be drawn
    /// a second time.
    pub fn clear(&mut self) {
        self.agent = None;
        self.text.clear();
    }
}

/// PURE, and the rule that makes streaming flicker-free (P3-D12): the durable `thought/text`
/// steps of a step index concatenate to a prefix of what streamed, so the trailing step renders
/// `live` whenever `live.len() >= durable.len()`, and the durable text otherwise.
pub fn trailing_text<'a>(durable: &'a str, live: &'a str) -> &'a str {
    if !live.is_empty() && live.len() >= durable.len() {
        live
    } else {
        durable
    }
}

/// PURE: who a stream should be teed for. `None` ⇒ delegate untouched.
///
/// The pane draws ONE agent, so a stream initiated by anyone else is not this pane's tail; a
/// stream with no ambient initiator is not attributable at all, and inventing an attribution is
/// exactly what §2 forbids.
pub fn tee_for(initiator: Option<AgentId>, focused: Option<&AgentId>) -> Option<AgentId> {
    let who = initiator?;
    match focused {
        Some(f) if *f != who => None,
        _ => Some(who),
    }
}

/// Wrap a stream so every [`Chunk::TextDelta`] lands in `live` and asks for a redraw. Every other
/// chunk passes through byte-identical: this is an OBSERVER, and the adapter downstream of it
/// must see exactly what it would have seen.
pub fn tee_stream(
    stream: bough_plugin_llm::LlmStream,
    live: Arc<Mutex<LiveText>>,
    agent: AgentId,
    redraw: Arc<dyn Fn() + Send + Sync>,
) -> bough_plugin_llm::LlmStream {
    stream
        .map(move |chunk| {
            if let Chunk::TextDelta { text } = &chunk {
                let mut held = live.lock();
                if held.agent.as_ref() != Some(&agent) {
                    // A new speaker: the previous tail belonged to someone else's turn.
                    held.text.clear();
                    held.agent = Some(agent.clone());
                }
                held.text.push_str(text);
                drop(held);
                redraw();
            }
            chunk
        })
        .boxed()
}

/// Install the tee on a filled [`StreamCall`], returning whether it teed.
///
/// Split out of the `llm/stream` listener so the whole decision — delegate untouched, or wrap —
/// is testable against a plain stream with no kernel and no adapter (§2.4's tee, in one function).
pub fn apply_tee(
    call: &StreamCall,
    initiator: Option<AgentId>,
    focused: Option<&AgentId>,
    live: Arc<Mutex<LiveText>>,
    redraw: Arc<dyn Fn() + Send + Sync>,
) -> bool {
    let Some(agent) = tee_for(initiator, focused) else {
        return false;
    };
    let Some(stream) = call.stream.take() else {
        // Nothing filled the slot. Putting a wrapper back over nothing would turn a downstream
        // `Chunk::Failed` into a hang.
        return false;
    };
    call.stream.put(tee_stream(stream, live, agent, redraw));
    true
}
