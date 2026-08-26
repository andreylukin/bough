//! Invariant: the dedupe guard asks the LEDGER, not the watermark. A `mail/delivered` step already
//! carrying this ref on this trajectory means the item is delivered, whatever the watermark says.
//! Run BEFORE the delivery; the watermark is written AFTER.

use bough_plugin_ledger::{LedgerHandle, Order, Ref, StepQuery, StepType, TrajId};

use crate::CollectError;

/// Has this trajectory already been delivered mail citing `r`?
///
/// P6-D15: the guard is per (trajectory, ref) — two agents configured for one repo each get their
/// own copy, and deduping globally would silently starve the second.
pub async fn already_delivered(
    ledger: &LedgerHandle,
    traj: &TrajId,
    r: &Ref,
) -> Result<bool, CollectError> {
    let hits = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj.clone()],
            kinds: vec![StepType::new("mail/delivered")],
            refs: vec![r.clone()],
            order: Order::SeqAsc,
            limit: Some(1),
            ..Default::default()
        })
        .await?;
    // The query is an ANY-match over the DERIVED refs, which is the indexed way to find the
    // candidates — but a `gh:o/r#12` in a check-run mail's `refs` (it is there for the router) is
    // not a delivery OF the PR. The delivered fact is what the step CITES, so the candidates are
    // narrowed to the steps that cite `r`.
    Ok(hits.iter().any(|s| s.cites.iter().any(|c| &c.r#ref == r)))
}
