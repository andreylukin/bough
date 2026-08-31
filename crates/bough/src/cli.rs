//! Invariant: every flag here selects or overlays CONFIG. None of them switches on a behaviour;
//! a behaviour is a row (§0.1 item 2).

use std::path::PathBuf;

/// The profile `--profile` defaults to. Named because `bough exec` has to be able to tell the
/// default apart from a profile the user typed.
pub const DEFAULT_PROFILE: &str = "tui";

/// The interactive teardown budget: how long the TUI may keep a user waiting after `/quit`
/// before the launcher restores the terminal and leaves anyway.
pub const DEFAULT_SHUTDOWN_MS: u64 = 2000;

/// The headless teardown budget (`bough exec`). Nobody is watching a terminal that has to come
/// back, and an aborted teardown leaves the ledger's write-ahead log on disk, so correctness
/// beats latency here: the deadline stays a hang backstop, but a much looser one.
pub const HEADLESS_SHUTDOWN_MS: u64 = 20_000;

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
    /// Compose in THIS terminal instead of attaching to the home's resident. The launcher-
    /// transport escape hatch (§0.1 item 2): the bare default invocation attaches; every explicit
    /// choice — this flag included — composes in-process.
    #[arg(long)]
    pub local: bool,
    /// Be the home's resident: compose headless and serve attached `bough` clients over
    /// `$BOUGH_HOME/tui.sock` (the `tui.attach` row). The bare `bough` spawns this itself when no
    /// resident is live; the flag's own effect on the launcher is only where the log goes.
    #[arg(long)]
    pub resident: bool,
    /// Override the embedded `profiles/` + `bundles/` directory.
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// How long teardown may take before the launcher restores the terminal and leaves anyway
    /// (phase ux1 §2.4, B8). Never a constant at the call site.
    #[arg(long, default_value_t = DEFAULT_SHUTDOWN_MS)]
    pub shutdown_ms: u64,
    /// `bough exec "<task>"`. A subcommand is COMPOSITION, not behaviour: it selects the headless
    /// profile and overlays one synthetic patch layer on the `exec` row (§0.1 item 2).
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// The launcher's subcommands.
///
/// Each one SELECTS the headless profile and writes ONE row's config. None of them names a plugin
/// type and none of them branches the boot path (§0.1 item 2).
#[derive(Debug, Clone, clap::Subcommand)]
pub enum Command {
    /// Run ONE task through the ordinary loop, print the answer, exit.
    Exec(ExecArgs),
    /// Stop the home's composing process (clean teardown) and start a fresh resident. A launcher
    /// transport verb like the attach flow itself (§0.1 item 2): it composes nothing in this
    /// process, so `main` intercepts it before boot.
    Restart,
    /// `bough mcp call <server> <tool> <json>` — one MCP tool call, printed, exit.
    Mcp(McpArgs),
    /// `bough wards test <file> [--since]` — dry-fire a ward against past ledger events.
    Wards(WardsArgs),
}

impl Command {
    /// The profile this subcommand forces. Every one of them is headless: a subcommand prints and
    /// exits, and a TUI would have nowhere to print to.
    pub fn profile(&self) -> &'static str {
        crate::exec::EXEC_PROFILE
    }
}

/// `bough mcp …`.
#[derive(Debug, Clone, clap::Args)]
pub struct McpArgs {
    #[command(subcommand)]
    pub command: McpCommand,
}

/// `bough mcp call`. Every field writes the `mcp.call` row's config and nothing else.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum McpCommand {
    /// Call one tool on one server and print the result.
    Call {
        /// The server row's name, as `mcp.rmcp`'s config spells it.
        server: String,
        /// The tool, as the server advertises it (no `mcp__` prefix).
        tool: String,
        /// The arguments, as a JSON object. Defaults to `{}`.
        #[arg(default_value = "{}")]
        args: String,
        /// `text` (the tool's text content) or `json` (the whole result).
        #[arg(long, value_enum)]
        print: Option<PrintFormat>,
        /// Stay up after the call instead of asking the process to exit.
        #[arg(long)]
        keep_running: bool,
    },
}

/// `bough wards …`.
#[derive(Debug, Clone, clap::Args)]
pub struct WardsArgs {
    #[command(subcommand)]
    pub command: WardsCommand,
}

/// `bough wards test`. Every field writes the `wards.test` row's config and nothing else.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum WardsCommand {
    /// Dry-fire a ward file against past ledger events and print the actions it WOULD take.
    Test {
        /// The ward file. Relative paths resolve against the process's working directory.
        file: String,
        /// How far back to replay: a ledger seq, or a duration like `24h`. Default: the whole
        /// tail the ledger will give.
        #[arg(long)]
        since: Option<String>,
        #[arg(long, value_enum)]
        print: Option<PrintFormat>,
        /// Stay up after the dry run instead of asking the process to exit.
        #[arg(long)]
        keep_running: bool,
    },
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

impl Cli {
    /// The profile this invocation actually composes under: a subcommand's, or `--profile`'s.
    ///
    /// It is read here rather than mutated into `self.profile` so `--dump-config` and boot agree
    /// without either of them having to remember to normalize first.
    pub fn effective_profile(&self) -> &str {
        match &self.command {
            Some(c) => c.profile(),
            None => &self.profile,
        }
    }

    /// What a subcommand implies beyond its own row: no patch watch, because the process exits
    /// when the row is done and a watch would only outlive it.
    ///
    /// A `--profile` the subcommand overrides is REPORTED, not swallowed: a flag that looks
    /// obeyed and is not is exactly the misconfiguration §0.2 refuses to hide. The clap default
    /// (`tui`) is indistinguishable from an explicit `--profile tui`, and a default is not a
    /// choice, so only a profile the user could only have typed is worth a word.
    pub fn normalize(&mut self) {
        let Some(cmd) = &self.command else { return };
        let forced = cmd.profile();
        if self.profile != forced && self.profile != DEFAULT_PROFILE {
            eprintln!(
                "bough: this subcommand runs under the `{forced}` profile; ignoring --profile {}",
                self.profile
            );
        }
        self.no_watch = true;
    }
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
