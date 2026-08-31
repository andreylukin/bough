//! Invariant: expansion is keyed by TOOL CALL ID, not by row index. Rows are recomputed from the
//! ledger on every append and paged in from underneath, so an index-keyed expansion would jump to
//! a different tool the moment anything arrived. Keyed by call id it simply survives.

use std::collections::BTreeSet;

use bough_plugin_llm::ToolCallId;
use bough_plugin_tui_shell::pane::{HitId, PaneOutcome};

/// The prefix a tool header's clickable region is minted under, spelled once.
pub const HIT_PREFIX: &str = "tool:";

/// The clickable region id for one tool call's header line.
pub fn hit_for_call(call: &ToolCallId) -> HitId {
    HitId::new(format!("{HIT_PREFIX}{call}"))
}

/// PURE: a clicked region ⇒ the call it belongs to. `None` for a region this pane did not mint.
pub fn call_of_hit(hit: &HitId) -> Option<ToolCallId> {
    let rest = hit.as_str().strip_prefix(HIT_PREFIX)?;
    if rest.is_empty() {
        return None;
    }
    Some(ToolCallId::new(rest))
}

/// Which tool calls are drawn expanded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Expanded(BTreeSet<ToolCallId>);

impl Expanded {
    /// Nothing expanded.
    pub fn new() -> Expanded {
        Expanded::default()
    }

    /// Flip one call. Returns the new state for that call.
    ///
    /// Closing a call also closes anything nested UNDER it: code mode mints an inner call as
    /// `{program}.{n}` (P-CM-D5), so a program's sub-rows are exactly the ids under its dotted
    /// prefix. Without this, collapsing a program and reopening it would show whichever sub-rows
    /// had been open before rather than the one line the row promises. An ordinary tool call has
    /// no such ids, so for every other row this is a no-op.
    pub fn toggle(&mut self, call: &ToolCallId) -> bool {
        if self.0.remove(call) {
            let prefix = format!("{call}.");
            self.0.retain(|c| !c.to_string().starts_with(&prefix));
            false
        } else {
            self.0.insert(call.clone());
            true
        }
    }

    pub fn insert(&mut self, call: &ToolCallId) {
        self.0.insert(call.clone());
    }

    pub fn is_expanded(&self, call: &ToolCallId) -> bool {
        self.0.contains(call)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// PURE: a click on a tool header ⇒ what the pane does. Split out of `Pane::handle` so it is
/// testable without a live shell.
pub fn on_click(expanded: &mut Expanded, hit: Option<&HitId>) -> PaneOutcome {
    match hit.and_then(call_of_hit) {
        Some(call) => {
            expanded.toggle(&call);
            PaneOutcome::Handled
        }
        None => PaneOutcome::Ignored,
    }
}
