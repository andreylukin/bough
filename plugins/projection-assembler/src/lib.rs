//! Invariant: this is the projection PROVIDER (§0.2). Context IS a projection of the ledger (§5):
//! this crate assembles it deterministically — no LLM in the request path — and degrades it in a
//! fixed order that is never silent for pins, digest or mail. It injects `ledger` and provides
//! `projection`; its bundle row is `projection-assembler`.
//!
//! SCAFFOLD: `unused_variables` and `dead_code` are allowed while the bodies are `todo!()` and the
//! private state they thread has no reader yet. Both allows go away with the last `todo!()`.
#![allow(unused_variables, dead_code)]

pub mod assemble;
pub mod bands;
pub mod degrade;
pub mod invariant;
pub mod registry;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::LedgerHandle;
use bough_plugin_projection::{
    AssembleRequest, Assembled, FileViewRequest, ProjectionError, Projector, SectionSpec,
    SectionToken,
};

use crate::registry::Registry;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "projection-assembler";

/// The row's config. Every deployment-varying number §5 names is a validated field here, never a
/// constant in the code (AGENTS.md).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssemblerConfig {
    /// The model's context window in tokens, before headroom.
    pub budget_tokens: usize,
    /// §5's headroom factor. 0.6 until a live measurement moves it (P1-D20).
    pub headroom: f32,
    /// How many steps the verbatim tail selects.
    pub tail_steps: usize,
    /// The floor §5 names: rung 2 never shrinks the tail below this.
    pub tail_floor_steps: usize,
    /// The "newest N" a collapsed mail header keeps.
    pub mail_newest_n: usize,
    /// Tiers above this are never rendered.
    pub max_tiers: u8,
    /// Where `write_file_view` puts a rendered trajectory.
    pub file_view_dir: PathBuf,
}

impl AssemblerConfig {
    /// PURE validation (§0.5): `0.0 < headroom <= 1.0`, `tail_floor_steps <= tail_steps`,
    /// `budget_tokens > 0`, `mail_newest_n > 0`. Anything else is a bundle typo and fails loud at
    /// compose.
    pub fn validate(&self) -> Result<(), ConfigError> {
        todo!("WP-5: AssemblerConfig::validate")
    }
}

/// The projector behind the `projection` binding.
pub struct Assembler {
    pub(crate) cfg: Arc<AssemblerConfig>,
    pub(crate) ledger: LedgerHandle,
    pub(crate) registry: Registry,
    /// The provider's captured context: the `projection/assemble` waterfall dispatches from it.
    pub(crate) ctx: Context,
}

impl Assembler {
    /// Build an assembler over an injected ledger.
    pub fn new(cfg: Arc<AssemblerConfig>, ledger: LedgerHandle, ctx: Context) -> Arc<Assembler> {
        todo!("WP-5: Assembler::new")
    }
}

#[async_trait::async_trait]
impl Projector for Assembler {
    fn provider(&self) -> &'static str {
        AssemblerPlugin::NAME
    }
    fn section(&self, spec: SectionSpec) -> Result<SectionToken, ProjectionError> {
        self.registry.add(spec)
    }
    async fn assemble(&self, req: &AssembleRequest) -> Result<Assembled, ProjectionError> {
        crate::assemble::assemble(self, req).await
    }
    async fn file_view(&self, req: &FileViewRequest) -> Result<String, ProjectionError> {
        todo!("WP-5: Assembler::file_view — trajectory_view + render_file_view, no writes")
    }
    async fn write_file_view(
        &self,
        req: &FileViewRequest,
        dir: Option<&Path>,
    ) -> Result<PathBuf, ProjectionError> {
        todo!("WP-5: Assembler::write_file_view — file_view plus one write")
    }
}

/// The provider plugin.
pub struct AssemblerPlugin;

#[async_trait::async_trait]
impl Plugin for AssemblerPlugin {
    const NAME: &'static str = "projection-assembler";
    type Config = AssemblerConfig;

    fn inject() -> Inject {
        Inject::required(["ledger"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        cfg.validate()
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!(
            "WP-5: AssemblerPlugin::apply — read `ledger`, build the Assembler, provide \
               `projection`, and defer the per-life invariant forget"
        )
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(AssemblerPlugin);
