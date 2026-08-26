//! Invariant (P5-D12): the child's request prefix is PINNED, and the pin is LEDGERED as
//! `fork/prefix`. §10 asks for the parent's request prefix byte-identical, and a child's own
//! projection cannot be that — its identity band names the CHILD and its verbatim tail carries the
//! `fork/end-seed` marker. The pin is an effect on the child's setup, so it unwinds with the child
//! and nothing global remembers it; the step is what keeps §0.2's "the sent request reconstructs
//! from the ledger" true THROUGH a pin: re-assembling `of_agent` at `as_of` reproduces it.

use bough_kernel::{Context, EffectHandle, PluginError};
use bough_plugin_ledger::{AgentName, Seq};
use bough_plugin_projection::Assembled;

/// Pin `prefix` for `child` as an effect of `ctx`, and append `fork/prefix`.
pub async fn pin(
    _ctx: &Context,
    _child: &AgentName,
    _prefix: Assembled,
    _of_agent: &AgentName,
    _as_of: Seq,
) -> Result<EffectHandle, PluginError> {
    todo!("WP-6: projection.pin_prefix as an effect, then append fork/prefix")
}
