//! Invariant: the dedupe guard asks the LEDGER, not the watermark. A `mail/delivered` step already
//! carrying this ref on this trajectory means the item is delivered, whatever the watermark says.
//! Run BEFORE the delivery; the watermark is written AFTER.
//!
//! MERGE (track B → Phase 5): the RULE now lives in `mail-router`, because the router is what
//! chooses recipients and a collector handing over an envelope does not know who will get it.
//! This is the same guard for the `deliver_to` FALLBACK path, delegating so the two cannot drift.

use bough_plugin_ledger::{LedgerHandle, Ref, TrajId};

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
    bough_plugin_mail_router::already_delivered(ledger, traj, r)
        .await
        .map_err(|e| CollectError::Mail(e.to_string()))
}
