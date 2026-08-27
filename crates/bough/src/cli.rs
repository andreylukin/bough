//! Invariant: every flag here selects or overlays CONFIG. None of them switches on a behaviour;
//! a behaviour is a row (§0.1 item 2).

use std::path::PathBuf;

/// The profile `--profile` defaults to. Named because `bough exec` has to be able to tell the
/// default apart from a profile the user typed.
pub const DEFAULT_PROFILE: &str = "tui";

/// `bough`.
#[derive(Debug, clap::Parser)]
#[command(name = "bough", version)]
pub struct Cli {
    /// Which profile to boot: `tui` (default), `headless`, `dev`.
    #[arg(long, default_value = DEFAULT_PROFILE)]
    pub profile: String,
    /// Extra patch layers, applied last, in argument order.
    #[arg(long = "patch")]
    pub patches: Vec<PathBuf>,
    /// Print the composed tree and exit 0. Never mounts anything.
    #[arg(long)]
    pub dump_config: bool,
    #[arg(long, value_enum, default_value = "yaml")]
    pub dump_format: DumpFormat,
    /// Boot, quiesce, assert, tear down, exit (Decision D15). No TUI, no watch. Used by the
    /// integration tests and by `scripts/audit-plugins.sh`.
    #[arg(long)]
    pub check: bool,
    /// Do not watch `~/.bough/bough.patch.yml`.
    #[arg(long)]
    pub no_watch: bool,
    /// Override the embedded `profiles/` + `bundles/` directory.
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// How long teardown may take before the launcher restores the terminal and leaves anyway
    /// (phase ux1 §2.4, B8). Never a constant at the call site.
    #[arg(long, default_value_t = 2000)]
    pub shutdown_ms: u64,
    /// `bough exec "<task>"`. A subcommand is COMPOSITION, not behaviour: it selects the headless
    /// profile and overlays one synthetic patch layer on the `exec` row (§0.1 item 2).
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// The launcher's subcommands.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum Command {
    /// Run ONE task through the ordinary loop, print the answer, exit.
    Exec(ExecArgs),
}

/// `bough exec` — every field here writes the `exec` row's config and nothing else.
#[derive(Debug, Clone, clap::Args)]
pub struct ExecArgs {
    /// The task, sent to the agent as an Andrey message.
    pub task: String,
    /// Which agent answers. Defaults to the `exec` row's configured agent.
    #[arg(long)]
    pub agent: Option<String>,
    /// Which trajectory the agent's chain lives on.
    #[arg(long)]
    pub traj: Option<String>,
    /// `text` (the last assistant text) or `json` (the whole wake).
    #[arg(long, value_enum)]
    pub print: Option<PrintFormat>,
    /// Stay up after the task instead of asking the process to exit. For a test that wants to
    /// inspect the running tree.
    #[arg(long)]
    pub keep_running: bool,
}

/// The CLI spelling of the `exec` row's `print` field. A separate enum for the same reason
/// [`DumpFormat`] is one: the launcher never names a plugin type (§0.1 item 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PrintFormat {
    Text,
    Json,
}

impl PrintFormat {
    /// The YAML spelling the `exec` row's config expects.
    pub fn as_str(self) -> &'static str {
        match self {
            PrintFormat::Text => "text",
            PrintFormat::Json => "json",
        }
    }
}

/// The CLI spelling of [`bough_kernel::DumpFormat`].
///
/// It is a separate enum for one reason: `clap::ValueEnum` would otherwise put `clap` in the
/// kernel's dependency list, and the kernel takes no CLI dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DumpFormat {
    Yaml,
    Json,
}

impl From<DumpFormat> for bough_kernel::DumpFormat {
    fn from(f: DumpFormat) -> Self {
        match f {
            DumpFormat::Yaml => bough_kernel::DumpFormat::Yaml,
            DumpFormat::Json => bough_kernel::DumpFormat::Json,
        }
    }
}

/// Everything that can go wrong before the tree is running.
#[derive(Debug, thiserror::Error)]
pub enum BootError {
    #[error("no profile named `{name}`; looked in {searched:?}")]
    UnknownProfile {
        name: String,
        searched: Vec<PathBuf>,
    },
    #[error("bundle `{name}` (profile `{profile}`) was not found in {searched:?}")]
    UnknownBundle {
        name: String,
        profile: String,
        searched: Vec<PathBuf>,
    },
    #[error("{path}: {detail}")]
    BadFile { path: PathBuf, detail: String },
    #[error(transparent)]
    Compose(#[from] bough_kernel::ComposeError),
    /// A compose failure that has ALREADY been broadcast as `config-update-failed`. The payload of
    /// that broadcast is an `Arc<ComposeError>` (§0.3), and this shares it rather than degrading
    /// the returned error to a rendered string.
    #[error("{0}")]
    ComposeShared(std::sync::Arc<bough_kernel::ComposeError>),
    #[error(transparent)]
    Catalog(#[from] bough_kernel::catalog::CatalogError),
    #[error(transparent)]
    Kernel(#[from] bough_kernel::KernelError),
    /// One or more enabled rows never activated. Printed row by row, after teardown (§0.2).
    #[error("{0} enabled row(s) never activated")]
    Unresolved(usize),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
