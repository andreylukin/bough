//! Invariant: these six tools are what makes a worker able to do a real task, and nothing here
//! reaches around the `tools` seam — each is an ordinary `Tool` registered through
//! `ToolsHandle::register`, guarded by the same pipeline as any other.

pub mod fs;
pub mod invariant;
pub mod spill;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Context, Plugin, PluginError};
use bough_plugin_tools::{
    RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome, ToolSpec,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tools-baseline";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BaselineConfig {
    /// The containment root (§7): a check, not a sandbox.
    pub root: PathBuf,
    pub bash_timeout_ms: u64,
    /// Output longer than this spills to a file with a locator inline.
    pub max_output_bytes: usize,
    pub max_read_bytes: usize,
    #[serde(default)]
    pub deny_globs: Vec<String>,
}

/// `bash` — Terminal render, never concurrency-safe.
pub struct Bash(pub Arc<BaselineConfig>);
/// `read_file` — Generic render, concurrency-safe.
pub struct ReadFile(pub Arc<BaselineConfig>);
/// `write_file` — Diff render, not concurrency-safe.
pub struct WriteFile(pub Arc<BaselineConfig>);
/// `edit_file` — Diff render, not concurrency-safe.
pub struct EditFile(pub Arc<BaselineConfig>);
/// `glob` — Generic render, concurrency-safe.
pub struct Glob(pub Arc<BaselineConfig>);
/// `grep` — Generic render, concurrency-safe.
pub struct Grep(pub Arc<BaselineConfig>);

macro_rules! baseline_tool {
    ($t:ty, $name:literal, $safe:literal, $wp:literal) => {
        #[async_trait::async_trait]
        impl Tool for $t {
            fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
                $safe
            }
            async fn call(
                &self,
                _call: Arc<ToolCall>,
                _cx: ToolCx,
            ) -> Result<ToolOutcome, ToolFailure> {
                todo!($wp)
            }
        }
    };
}

baseline_tool!(
    Bash,
    "bash",
    false,
    "WP-3: run the command under the row's timeout"
);
baseline_tool!(
    ReadFile,
    "read_file",
    true,
    "WP-3: contained read, bounded by max_read_bytes"
);
baseline_tool!(WriteFile, "write_file", false, "WP-3: contained write");
baseline_tool!(
    EditFile,
    "edit_file",
    false,
    "WP-3: contained exact-string edit"
);
baseline_tool!(Glob, "glob", true, "WP-3: contained glob");
baseline_tool!(Grep, "grep", true, "WP-3: contained regex search");

/// The six specs this row registers, with their render intents.
///
/// WP-3.
pub fn specs(_cfg: Arc<BaselineConfig>) -> Vec<ToolSpec> {
    todo!("WP-3: bash Terminal, read_file/glob/grep Generic, write_file/edit_file Diff")
}

/// Named so the render intents are visible in the scaffold rather than only in `specs`.
pub const RENDER_INTENTS: &[(&str, RenderIntent)] = &[
    ("bash", RenderIntent::Terminal),
    ("read_file", RenderIntent::Generic),
    ("write_file", RenderIntent::Diff),
    ("edit_file", RenderIntent::Diff),
    ("glob", RenderIntent::Generic),
    ("grep", RenderIntent::Generic),
];

/// The consumer row.
pub struct BaselineToolsPlugin;

#[async_trait::async_trait]
impl Plugin for BaselineToolsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = BaselineConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["tools"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-3: register the six tools and the spill listener, all as effects")
    }
}

bough_kernel::register_plugin!(BaselineToolsPlugin);
