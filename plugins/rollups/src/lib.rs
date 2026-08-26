//! Invariant: this crate is the rollups SERVICE DEFINITION (§0.2, P4-D1). A sealed rollup is
//! IMMUTABLE (§3): a raw segment is summarized exactly once, a block is stamped with the
//! `prompt_ver` and `sealed_at` that produced it, and the only write a sealed row ever accepts
//! afterwards is `superseded_by`, set once. Tiers are an INDEX, never a replacement: every block
//! carries refs into the layer beneath it, so a coarse block resolves to raw evidence.
//!
//! This crate owns the key, the vocabulary, the pure algorithms and the provider-conformance
//! suite, and not one line of a summarizer. It has no `Plugin` impl and no bundle row; its
//! invariant specs are returned by the PROVIDERS' `Plugin::invariants()`.

pub mod block;
pub mod conformance;
pub mod error;
pub mod expiry;
pub mod invariant;
pub mod plan;
pub mod request;
pub mod window;

use std::sync::Arc;

use bough_kernel::ServiceKey;

pub use block::{
    notable_refs, refs_of, Beneath, DigestBlock, Standing, Theme, TierBlock, WindowRef,
};
pub use error::RollupsError;
pub use expiry::{Expired, NEVER_EXPIRABLE};
pub use plan::{coverage, is_ours, plan, tier_id, TierCfg};
pub use request::{
    Attribution, DigestReport, DigestRequest, Inputs, PassId, PlannedBlock, SealPlan, SealReport,
    SealRequest, Skip, SkipReason, Stop, SupersedeReport, SupersedeRequest,
};
pub use window::{windows, Cut, Window, WindowCfg};

/// The `rollups` service key.
pub struct Rollups;

impl ServiceKey for Rollups {
    type Value = RollupsHandle;
    const NAME: &'static str = "rollups";
}

/// The concrete handle newtype the key's value is (Decision D5, the `LedgerHandle` precedent).
#[derive(Clone)]
pub struct RollupsHandle(pub Arc<dyn Summarizer>);

/// What a rollups provider does.
///
/// Every method is idempotent under a repeated call with the same request: [`Summarizer::seal`]
/// re-run over an unchanged ledger seals nothing and reports [`Stop::NothingToDo`].
#[async_trait::async_trait]
pub trait Summarizer: Send + Sync + 'static {
    /// Catalog name of the plugin behind this binding; the swap test reads it.
    fn provider(&self) -> &'static str;

    /// The `prompt_ver` this provider stamps on what it seals. `""` iff it seals nothing.
    fn prompt_ver(&self) -> &str;

    /// PURE with respect to the world: reads the ledger, calls no model, writes nothing.
    /// What a [`Summarizer::seal`] would do, and why each skipped range was skipped.
    async fn plan(&self, req: &SealRequest) -> Result<SealPlan, RollupsError>;

    /// Execute the plan: map over episode windows, reduce to themes, seal each block, append one
    /// `rollup/request` per model call and one `rollup/sealed` per block.
    async fn seal(&self, req: &SealRequest) -> Result<SealReport, RollupsError>;

    /// The relief valve (§3, §8): mint generation n+1 over the SAME range, set `superseded_by` on
    /// generation n, append the expiry note. Refused when the block is already superseded.
    async fn supersede(&self, req: &SupersedeRequest) -> Result<SupersedeReport, RollupsError>;

    /// Rebuild an agent's standing digest FROM RAW EVIDENCE. Sealed tiers are read, never
    /// re-summarized and never re-sealed (§8). Supersedes the previous digest and repoints
    /// `agents.digest_rollup`.
    async fn rebuild_digest(&self, req: &DigestRequest) -> Result<DigestReport, RollupsError>;
}
