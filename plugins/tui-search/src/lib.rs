//! Invariant: this row is the SWAP subject and is deliberately self-contained. Nothing else in the
//! tree may depend on it, and disabling it by patch must be indistinguishable from never having
//! mounted it — no pane, no listener, no binding left behind (§17 Phase 3).
//!
//! It reads `ctx.ledger` and never `ctx.agents`' loop: a hit is a step id, and clicking one is a
//! `FocusRequest`, not a wake.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::{AgentName, AgentRow, SearchHit, Seq, StepId, StepType, TrajId};
use bough_plugin_tui_shell::pane::{Pane, PaneCx, PaneEvent, PaneOutcome, RenderCx};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-search";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    pub height: u16,
    pub limit: usize,
    pub debounce_ms: u64,
}

/// One rendered hit.
#[derive(Clone, Debug, PartialEq)]
pub struct HitRow {
    /// Resolved traj → `agents` row; `None` for a rowless traj.
    pub agent: Option<AgentName>,
    pub traj: TrajId,
    pub step: StepId,
    pub seq: Seq,
    pub kind: StepType,
    pub snippet: String,
}

/// PURE: `SearchHit` + the agents rows ⇒ display rows (agent, seq, kind, snippet).
pub fn hit_rows(_hits: &[SearchHit], _agents: &[AgentRow]) -> Vec<HitRow> {
    todo!("WP-5")
}

/// The pane: a one-line input it owns (the composer belongs to the shell), debounced, over
/// `LedgerStore::search`.
pub struct SearchPane {
    _private: (),
}

#[async_trait::async_trait]
impl Pane for SearchPane {
    fn render(&self, _cx: &mut RenderCx<'_>) {
        todo!("WP-5")
    }

    async fn handle(&self, _ev: PaneEvent, _cx: PaneCx) -> PaneOutcome {
        todo!("WP-5: typing debounces a query; a click on a hit returns Focus(step)")
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        todo!("WP-5")
    }
}

/// The row.
pub struct SearchPlugin;

#[async_trait::async_trait]
impl Plugin for SearchPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = SearchConfig;

    fn inject() -> Inject {
        Inject::required(["tui", "ledger"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-5: register the pane as an effect")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(SearchPlugin);
