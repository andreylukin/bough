//! Invariant: the pass appends, and only appends. Every block is written with `seal_rollup` under
//! a synthetic pass wake (P4-D2), one `rollup/request` per model call and one `rollup/sealed` per
//! block; nothing above `upto` is touched, and nothing within `seal_lag_steps` of the head is
//! sealed (P4-D11), so a sealed tier and the verbatim tail never describe the same steps.

use bough_plugin_rollups::{
    RollupsError, SealPlan, SealReport, SealRequest, SupersedeReport, SupersedeRequest,
};

use crate::SummarizerInner;

/// Plan a pass: pure with respect to the world (reads the ledger, calls no model, writes nothing).
pub async fn plan(_inner: &SummarizerInner, _req: &SealRequest) -> Result<SealPlan, RollupsError> {
    todo!("WP-2: planning")
}

/// Run a pass to its budget.
pub async fn run(_inner: &SummarizerInner, _req: &SealRequest) -> Result<SealReport, RollupsError> {
    todo!("WP-2: the map/reduce pass")
}

/// Supersede one block at generation n+1 and append the `memory/expired` note naming the old one.
pub async fn supersede(
    _inner: &SummarizerInner,
    _req: &SupersedeRequest,
) -> Result<SupersedeReport, RollupsError> {
    todo!("WP-2: supersession")
}
