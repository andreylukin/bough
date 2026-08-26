//! Invariant: every refusal this seam can utter names the block it is about, so a caller never
//! has to guess whether a seal was skipped, refused or lost.

use bough_plugin_ledger::{LedgerError, RollupId, Seq, TrajId};

/// Everything the rollups seam can go wrong as.
#[derive(Debug, thiserror::Error)]
pub enum RollupsError {
    /// THE seal-once refusal: this `(traj, tier, from, to)` is already covered.
    #[error("tier {tier} range {}..{} of `{traj}` is already sealed as `{existing}`", .from.0, .to.0)]
    AlreadySealed {
        traj: TrajId,
        tier: u8,
        from: Seq,
        to: Seq,
        existing: RollupId,
    },
    #[error("rollup `{0}` is not in the ledger")]
    NotFound(RollupId),
    #[error("rollup `{0}` is already superseded by `{1}`")]
    AlreadySuperseded(RollupId, RollupId),
    #[error("`{0}` is not a block this provider sealed; supersession is namespaced")]
    NotOurs(RollupId),
    #[error("the model returned no usable block: {0}")]
    BadBlock(String),
    #[error("the model call failed: {0}")]
    Model(String),
    /// A provider that seals nothing (`rollups-none`) refuses every write, and SAYS so rather
    /// than reporting a success it did not perform (§16).
    #[error("this provider seals nothing: {0}")]
    Refused(String),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}
