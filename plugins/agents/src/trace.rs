//! Invariant: teardown ORDER is a fact a test can read. §2 fixes it at stop+drain → unwind scope
//! → detach agent → detach session; the four stages record themselves here, keyed by agent, and a
//! test that also records from its own driver and from a scope inverse sees whether the labels
//! are truthful.
//!
//! It is a test-facing observation channel, never an input to a decision. Keyed by agent so two
//! tests tearing down concurrently in one binary cannot read each other's trace.

use std::collections::BTreeMap;

use parking_lot::Mutex;

use crate::ids::AgentId;

static TRACE: Mutex<BTreeMap<String, Vec<String>>> = Mutex::new(BTreeMap::new());

/// Record one stage for `agent`.
pub fn push(agent: &AgentId, stage: impl Into<String>) {
    TRACE
        .lock()
        .entry(agent.to_string())
        .or_default()
        .push(stage.into());
}

/// Everything recorded for `agent`, oldest first.
pub fn seen(agent: &AgentId) -> Vec<String> {
    TRACE
        .lock()
        .get(agent.as_str())
        .cloned()
        .unwrap_or_default()
}

/// Forget `agent`'s trace.
pub fn forget(agent: &AgentId) {
    TRACE.lock().remove(agent.as_str());
}
