//! Invariant: this provider seals NOTHING, ever, and says so. `plan` reports every candidate as
//! [`SkipReason::Refused`], `seal` reports [`Stop::NothingToDo`], and `supersede` /
//! `rebuild_digest` return [`RollupsError::Refused`]. It appends no step and makes no model call,
//! which is what makes it a truthful stub rather than a slow one — the swap test reads exactly
//! that difference.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_rollups::{
    DigestReport, DigestRequest, RollupsError, SealPlan, SealReport, SealRequest, Summarizer,
    SupersedeReport, SupersedeRequest,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "rollups-none";

/// The stub's config: nothing to tune, so the swap patch is `config: {}`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoneConfig {}

/// The stub summarizer.
#[derive(Clone)]
pub struct NoneSummarizer {
    pub ledger: bough_plugin_ledger::LedgerHandle,
}

#[async_trait::async_trait]
impl Summarizer for NoneSummarizer {
    fn provider(&self) -> &'static str {
        PLUGIN_NAME
    }

    /// `""`: this provider stamps nothing because it seals nothing.
    fn prompt_ver(&self) -> &str {
        ""
    }

    async fn plan(&self, _req: &SealRequest) -> Result<SealPlan, RollupsError> {
        todo!("WP-6: an honest plan whose every candidate is Refused")
    }

    async fn seal(&self, _req: &SealRequest) -> Result<SealReport, RollupsError> {
        todo!("WP-6: Stop::NothingToDo, no step, no call")
    }

    async fn supersede(&self, _req: &SupersedeRequest) -> Result<SupersedeReport, RollupsError> {
        todo!("WP-6: RollupsError::Refused")
    }

    async fn rebuild_digest(&self, _req: &DigestRequest) -> Result<DigestReport, RollupsError> {
        todo!("WP-6: RollupsError::Refused")
    }
}

/// The stub provider row.
pub struct RollupsNonePlugin;

#[async_trait::async_trait]
impl Plugin for RollupsNonePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = NoneConfig;

    fn inject() -> Inject {
        // `ledger` only — it needs the store to answer `plan` honestly — so the swap changes no
        // other row's satisfaction.
        Inject::required(["ledger"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-6: provide `rollups` with the stub")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![
            bough_plugin_rollups::invariant::seal_once(),
            bough_plugin_rollups::invariant::tiers_are_an_index(),
        ]
    }
}

bough_kernel::register_plugin!(RollupsNonePlugin);
