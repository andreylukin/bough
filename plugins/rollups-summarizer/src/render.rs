//! Invariant: rendering is PURE, which is what makes the offline suite deterministic. And the
//! index never depends on the model's discipline: a model that returns prose and no structure
//! still yields a block whose `evidence` comes from the WINDOW, not from the answer.

use bough_plugin_ledger::{Rollup, Step};
use bough_plugin_rollups::{Inputs, RollupsError, TierBlock, Window};

use crate::call::Phase;
use crate::SummarizerConfig;

/// The recap prompt, versioned.
///
/// `None` when the binary has no prompt for `(phase, ver)`; [`crate::resolve::validate`] turns
/// that into a boot refusal.
pub fn system_prompt(phase: Phase, ver: &str) -> Option<&'static str> {
    crate::prompts::lookup(phase, ver)
}

/// One episode window as the model sees it: `[seq] kind: one line`, thoughts marked as thoughts,
/// evidence carrying its cites.
pub fn render_window(_steps: &[Step], _w: &Window) -> String {
    todo!("WP-2: window rendering")
}

/// `fanout` child blocks as the reduce sees them.
pub fn render_children(_children: &[Rollup]) -> String {
    todo!("WP-2: child rendering")
}

/// Parse the model's answer into a block.
pub fn parse_block(
    _answer: &str,
    _inputs: &Inputs,
    _steps: &[Step],
    _cfg: &SummarizerConfig,
) -> Result<TierBlock, RollupsError> {
    todo!("WP-2: answer → TierBlock")
}
