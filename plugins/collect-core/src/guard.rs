//! Invariant: the dedupe guard asks the LEDGER, not the watermark. A `mail/delivered` step already
//! carrying this ref on this trajectory means the item is delivered, whatever the watermark says.
//! Run BEFORE the delivery; the watermark is written AFTER.

use bough_plugin_ledger::{LedgerHandle, Ref, TrajId};

use crate::CollectError;

/// Has this trajectory already been delivered mail citing `r`? WP-2.
pub async fn already_delivered(
    ledger: &LedgerHandle,
    traj: &TrajId,
    r: &Ref,
) -> Result<bool, CollectError> {
    let _ = (ledger, traj, r);
    todo!("WP-2: a bounded `StepQuery` over `mail/delivered` refs on this trajectory")
}
