//! Invariant: every flag here selects or overlays CONFIG. None of them switches on a behaviour;
//! a behaviour is a row (§0.1 item 2).

use std::path::PathBuf;

/// `bough`.
#[derive(Debug, clap::Parser)]
#[command(name = "bough", version)]
pub struct Cli {
    /// Which profile to boot: `tui` (default), `headless`, `dev`.
    #[arg(long, default_value = "tui")]
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
    #[error(transparent)]
    Kernel(#[from] bough_kernel::KernelError),
    /// One or more enabled rows never activated. Printed row by row, after teardown (§0.2).
    #[error("{0} enabled row(s) never activated")]
    Unresolved(usize),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
