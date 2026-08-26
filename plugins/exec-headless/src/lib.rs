//! Invariant: `bough exec` runs ONE task through the ordinary loop and then asks the process to
//! exit. It is composition, not behaviour: it resumes-or-creates an agent, sends the task as an
//! ANDREY message (so §5's answer-wake rule applies unchanged), awaits `when_idle()`, prints, and
//! calls `Kernel::request_exit` — the launcher still owns the exit path and tears down first.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Plugin, PluginError};

/// The catalog name of this row. `exec`, not `exec-headless`: the row is what the profile names.
pub const PLUGIN_NAME: &str = "exec";

/// How the result is printed.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Print {
    /// The last assistant text.
    Text,
    /// The whole wake, as JSON.
    Json,
}

/// The row's config. `bough exec "<task>"` sets `task` through one synthetic patch layer.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecConfig {
    /// Empty ⇒ the row mounts and does nothing, which is what makes the headless profile usable
    /// without a task.
    #[serde(default)]
    pub task: String,
    pub agent: String,
    pub traj: String,
    pub print: Print,
    /// `false` leaves the process running after the task, for a test that wants to inspect it.
    pub exit_when_idle: bool,
}

/// The surface row.
pub struct ExecPlugin;

#[async_trait::async_trait]
impl Plugin for ExecPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ExecConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["agents", "ledger"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-8: resume-or-create, send as Andrey, when_idle, print, request_exit")
    }
}

bough_kernel::register_plugin!(ExecPlugin);
