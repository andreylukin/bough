//! Invariant: the six built-in bands render from the ledger and NOTHING ELSE, in `Slot` order, and
//! each works with ZERO rollups — Phase 4 produces tiers and digests, so a band with no input
//! renders nothing at all rather than an empty header. The tail is de-interleaved by `wake_id`
//! (§3): the window is selected by seq, then grouped by wake, wakes ordered by their first
//! selected seq, seq order preserved inside a wake — a pure function of the rows.

use bough_plugin_projection::{ProjectionError, RenderedSection, SectionRequest};

use crate::AssemblerConfig;

/// **Identity** — the `agents` row plus the digest pointer. The about-line's state half arrives in
/// Phase 2 as a contributed section at `Position { Identity, After }` (P1-D12).
pub async fn identity(
    req: &SectionRequest,
    cfg: &AssemblerConfig,
) -> Result<Option<RenderedSection>, ProjectionError> {
    todo!("WP-5: bands::identity")
}

/// **Pins** — `live_pins(connected)`, verbatim, oldest first, each with its step id. Never
/// filtered by age, never demoted (§5).
pub async fn pins(
    req: &SectionRequest,
    cfg: &AssemblerConfig,
) -> Result<Option<RenderedSection>, ProjectionError> {
    todo!("WP-5: bands::pins")
}

/// **Digest** — the agent's `digest_rollup`, if any. With zero rollups: nothing, and no header.
pub async fn digest(
    req: &SectionRequest,
    cfg: &AssemblerConfig,
) -> Result<Option<RenderedSection>, ProjectionError> {
    todo!("WP-5: bands::digest")
}

/// **Tiers** — kind `tier`, COARSE TO FINE, tier ≤ `max_tiers`, kept when
/// `notable_refs ∩ agent.refs ≠ ∅` **or** `notable_refs` is empty (P1-D13).
pub async fn tiers(
    req: &SectionRequest,
    cfg: &AssemblerConfig,
) -> Result<Vec<RenderedSection>, ProjectionError> {
    todo!("WP-5: bands::tiers")
}

/// **Tail** — the newest `tail_steps` steps of the agent's own chain, verbatim, oldest first,
/// de-interleaved by `wake_id`.
pub async fn tail(
    req: &SectionRequest,
    cfg: &AssemblerConfig,
) -> Result<Option<RenderedSection>, ProjectionError> {
    todo!("WP-5: bands::tail")
}

/// Group a selected window into wake blocks: wakes ordered by their first selected seq, seq order
/// preserved inside a wake. Pure — no clock, no arrival order.
pub fn de_interleave(steps: &[bough_plugin_ledger::Step]) -> Vec<Vec<bough_plugin_ledger::Step>> {
    todo!("WP-5: bands::de_interleave")
}

/// **Mail** — `unconsumed_mail`, newest first, grouped by class.
pub async fn mail(
    req: &SectionRequest,
    cfg: &AssemblerConfig,
) -> Result<Option<RenderedSection>, ProjectionError> {
    todo!("WP-5: bands::mail")
}
