//! Invariant (§5, P2-D20): `request/header` is appended ONLY when it differs from the last one in
//! this wake, and it carries the composition fingerprint, the projection digest, `as_of` and the
//! budget — the four things that turn V4 into a reconstruction rather than a hash comparison.

use std::sync::Arc;

use bough_plugin_ledger::Seq;
use bough_plugin_llm::{CallConfig, LlmRequest, RequestFacts};
use bough_plugin_projection::Assembled;
use bough_plugin_tools::LlmToolDef;

/// Everything one request is built from, gathered before any of it is written down.
pub struct RequestInputs {
    pub facts: Arc<RequestFacts>,
    pub projection: Assembled,
    pub as_of: Seq,
    pub budget: usize,
    pub tools: Vec<LlmToolDef>,
    pub call: CallConfig,
}

/// Build the request. Pure over its inputs — no clock, no ledger — so the invariant's
/// reconstruction runs the same function on the same inputs.
///
/// WP-4.
pub fn build(_inputs: &RequestInputs, _messages: Vec<bough_plugin_llm::LlmMessage>) -> LlmRequest {
    todo!("WP-4: projection -> system, transcript -> messages, tools + call as given")
}

/// The `request/header` body for a request, or `None` when it repeats the last one in this wake.
///
/// WP-4.
pub fn header_if_changed(
    _last: Option<&bough_plugin_ledger::vocabulary::RequestHeader>,
    _inputs: &RequestInputs,
) -> Option<bough_plugin_ledger::vocabulary::RequestHeader> {
    todo!("WP-4: build the header, compare with the last, append only on a change")
}
