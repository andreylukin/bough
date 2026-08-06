//! The cheap tier (port of `src/worker/`): auto titles, composer ghost text,
//! live activity blurbs over `complete_text`. Not an agent. Each method
//! resolves `None` and never errors; one in-flight blurb per session, drop
//! don't queue. **v1 ships `cheap: None`** — every reader degrades on absence
//! by contract (ARCHITECTURE.md §4.3). STUB (wave 2, row 2.19).

use std::sync::Arc;

use crate::types::CheapTier;

/// The v1 answer: no cheap tier. Every reader degrades on `None`.
pub fn create_cheap_tier() -> Option<Arc<dyn CheapTier>> {
    None
}

/// The env var the cheap tier's model id is read from, per call — never from
/// `ctx.model` (spec §12: the two tiers are chosen separately).
pub const CHEAP_MODEL_ENV: &str = "BOUGH_CHEAP_MODEL";

/// The floor when the picker has never been used. Small, hosted, and fast.
pub const DEFAULT_CHEAP_MODEL: &str = "claude-haiku-4-5";

/// The cheap tier's model id (TS `worker/titles.ts::cheapModel`). Read fresh
/// per call so a changed env var takes effect without a restart in tests.
pub fn cheap_model() -> String {
    std::env::var(CHEAP_MODEL_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_CHEAP_MODEL.to_string())
}
