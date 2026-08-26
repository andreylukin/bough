//! Invariant: one pass, three appends and one seam call — and nothing else. Distillation goes
//! through `ctx.rollups.rebuild_digest(DigestRequest { from_raw: false, .. })`, so "reconsolidation
//! adds a block" and "the summarizer seals a block" are ONE code path and cannot disagree about
//! `prompt_ver`, `sealed_at` or the `rollup/sealed` step (P4-D6).

use crate::{PassPlan, PassReport, PassRequest, ReconError, ReconInner};

/// What a pass WOULD do. No model call, no write.
pub async fn plan(_inner: &ReconInner, _req: &PassRequest) -> Result<PassPlan, ReconError> {
    todo!("WP-3: pass planning")
}

/// Run the pass.
pub async fn run(_inner: &ReconInner, _req: &PassRequest) -> Result<PassReport, ReconError> {
    todo!("WP-3: the reconsolidation pass")
}
