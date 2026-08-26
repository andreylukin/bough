//! Invariant: this is a SECOND Provider of the same seam, and it is the phase's swap gate. It
//! honours every waterfall and appends every durable step in §5's order — and it implements
//! neither preemption nor retry nor drain debouncing, because a replacement loop is held to the
//! LEDGER PROTOCOL and not to a feature list.

pub mod invariant;
pub mod script;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::{AgentCell, AgentDriver, AgentError, AgentFactory, Attach};

pub use script::{Script, ScriptedStep, ScriptedWake};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "agent-loop-scripted";

/// The row's config: a transcript file, or the wakes inline.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScriptedConfig {
    #[serde(default)]
    pub transcript: Option<PathBuf>,
    /// Raw JSON, parsed by [`Script::parse`]; the config schema stays shallow on purpose.
    #[serde(default)]
    pub wakes: Option<serde_json::Value>,
    /// `true`: running out of script is an error, not a silent idle.
    #[serde(default = "yes")]
    pub strict: bool,
}

fn yes() -> bool {
    true
}

/// The factory this row registers.
pub struct ScriptedFactory {
    _cfg: Arc<ScriptedConfig>,
}

#[async_trait::async_trait]
impl AgentFactory for ScriptedFactory {
    fn driver(&self) -> &'static str {
        PLUGIN_NAME
    }

    /// WP-5.
    async fn attach(
        &self,
        _cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        todo!("WP-5: a driver that replays the script through the same seam")
    }
}

/// The Provider row. In the catalog, in NO bundle: the swap patch names it.
pub struct ScriptedLoopPlugin;

#[async_trait::async_trait]
impl Plugin for ScriptedLoopPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ScriptedConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["agents", "ledger", "projection", "tools"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-5: agents.set_factory(ScriptedFactory) — the slot the swap test frees")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::requests_reconstruct_from_the_ledger()]
    }
}

bough_kernel::register_plugin!(ScriptedLoopPlugin);
