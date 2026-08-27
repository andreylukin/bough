//! Invariant: this row is a `tools` CONSUMER, and the `tools` seam is untouched by it. It
//! registers exactly ONE tool — `run(program)` — and every call a program makes goes through the
//! seam's own pipeline, lands as a ledgered sub-step, and is subject to scope shadowing and
//! `restrict` exactly as a typed call is. Model-visible ⟺ ledgered holds by construction: the
//! only thing the model gets back is console output, and console output is itself a step.

pub mod bind;
pub mod conceal;
pub mod console;
pub mod invariant;
pub mod run;
pub mod surface;
pub mod vocabulary;

use std::collections::BTreeMap;
use std::sync::Arc;

use bough_kernel::{Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_js::Caps;

pub use conceal::{ConcealMode, Mirror};
pub use vocabulary::{
    ProgramCallBody, ProgramConsoleBody, ProgramErrorBody, ProgramResultBody,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tools-codemode";

/// The ONE API tool. A protocol constant, not config: the TUI, the bench and the surface section
/// all key on it.
pub const RUN_TOOL: &str = "run";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodemodeConfig {
    /// `None` ⇒ the `js` row's `default_caps`.
    #[serde(default)]
    pub caps: Option<Caps>,
    #[serde(default)]
    pub conceal: ConcealMode,
    /// JS name → registered `ToolName`. Ships as `{claim: propose_claim, agent: spawn_worker}`.
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    /// JS namespace → `ToolName` prefix. Ships as `{mcp: "mcp__", act: ""}`.
    #[serde(default)]
    pub namespaces: BTreeMap<String, String>,
    pub max_console_bytes: usize,
    pub max_calls_per_program: u32,
    /// `bash`/`sh` legs must carry 3–5 tags. `false` only for the bench's control arm.
    pub tags_required: bool,
    /// Register the surface documentation as a projection section. `false` for tests that build
    /// the request themselves.
    pub surface_section: bool,
}

/// The Consumer row.
pub struct CodemodePlugin;

#[async_trait::async_trait]
impl Plugin for CodemodePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = CodemodeConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["tools", "js", "ledger", "agents", "projection"])
            .union(&bough_kernel::Inject::optional(["approval"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        if cfg.max_console_bytes == 0 || cfg.max_calls_per_program == 0 {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: "max_console_bytes and max_calls_per_program must be at least 1"
                    .to_string(),
            });
        }
        Ok(())
    }

    /// Registers, as effects: the `run` spec; the concealment (at apply for every live agent and
    /// on `agents::AgentCreated`); the four step types; the `codemode.surface` section; and its
    /// invariant.
    ///
    /// WP-2 owns the body; WP-5 adds the section.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-2: register `run`, the concealment, the step types and the surface section")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::every_program_call_is_ledgered()]
    }
}

bough_kernel::register_plugin!(CodemodePlugin);
