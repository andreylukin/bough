//! Invariant: this crate is the commands SERVICE DEFINITION (§11, §0.2). It owns the `commands`
//! key, the registry, the pure `parse`, scoped resolution with most-specific-wins, `dispatch` and
//! the one observability event — and NOT ONE surface. It never renders, never touches a terminal
//! and never starts a wake: a dispatch appends no step (P3-D8) and reaches a model only by way of
//! `Agent::inject` / `Agent::steer`, which are durable already.
//!
//! It holds live state (the registry), so it IS a catalog row and provides its own key.

pub mod invariant;
pub mod parse;

use std::sync::Arc;

use bough_kernel::{
    Context, EffectHandle, EmitEvent, Inject, InvariantSpec, Plugin, PluginError, ServiceKey,
};
use bough_plugin_agents::Agent;
use bough_plugin_ledger::{AgentName, Cite};
use chrono::{DateTime, Utc};

pub use parse::parse;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "commands";

/// The `commands` service key.
pub struct Commands;

impl ServiceKey for Commands {
    type Value = CommandsHandle;
    const NAME: &'static str = "commands";
}

bough_util::brand_id! {
    /// A command's name, without the prefix: `focus`, `help`, `oldfeed`.
    pub struct CommandName;
}

/// Where a command is visible. Most-specific-wins on a name clash.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CommandScope {
    /// Every agent.
    Global,
    /// One agent only.
    Agent(AgentName),
}

/// One registered command.
#[derive(Clone)]
pub struct CommandSpec {
    pub name: CommandName,
    /// One line, for `/help` and completion.
    pub summary: String,
    /// `"/focus <agent>"`.
    pub usage: String,
    /// Structured args, so Phase 6's `bough mcp call` can validate without a parser of its own.
    pub args: schemars::Schema,
    pub scope: CommandScope,
    pub run: Arc<dyn Command>,
}

/// The listing shape `list()` returns: a spec without its runnable half.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandInfo {
    pub name: CommandName,
    pub summary: String,
    pub usage: String,
    pub scope: CommandScope,
}

/// What a command does.
#[async_trait::async_trait]
pub trait Command: Send + Sync + 'static {
    async fn run(&self, inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError>;
}

/// One typed command line, as `parse` produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct Invocation {
    pub name: CommandName,
    /// The whole line as typed, prefix included.
    pub raw: String,
    /// Shell-style split of the remainder; quoted runs stay whole.
    pub args: Vec<String>,
}

/// What a command produced. Rendered locally; never a step (P3-D8).
#[derive(Clone, Debug, PartialEq)]
pub struct CommandOutput {
    pub text: String,
    pub render: OutputRender,
    /// A command MAY cite; the pane renders cites under the output.
    pub cites: Vec<Cite>,
}

/// How a surface renders a command's output.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputRender {
    Plain,
    KeyValue,
    Terminal,
}

/// Everything a dispatch can go wrong as.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum CommandError {
    #[error("unknown command `{name}`{}", suggestion_suffix(.did_you_mean))]
    Unknown {
        name: String,
        did_you_mean: Option<String>,
    },
    #[error("usage: {usage}")]
    BadArgs { usage: String, detail: String },
    #[error("{0}")]
    Failed(String),
}

/// `" — did you mean `focus`?"`, or nothing.
pub fn suggestion_suffix(did_you_mean: &Option<String>) -> String {
    match did_you_mean {
        Some(s) => format!(" — did you mean `{s}`?"),
        None => String::new(),
    }
}

/// What a command runs against.
pub struct CommandCx {
    pub ctx: Context,
    /// The focused agent, when there is one. A command may steer/inject through it; that is
    /// durable (`inbox/spliced`) and is the ONLY way a command reaches a model.
    pub agent: Option<Agent>,
    pub at: DateTime<Utc>,
}

/// The concrete handle the key's value is.
#[derive(Clone)]
pub struct CommandsHandle(pub Arc<CommandsInner>);

/// The registry's live state.
pub struct CommandsInner {
    _private: (),
}

impl CommandsHandle {
    /// An empty registry.
    pub fn new(_ctx: Context, _cfg: Arc<CommandsConfig>) -> CommandsHandle {
        todo!("WP-1: the registry's live state")
    }

    /// An EFFECT (§0.2): unloading the registering row removes the command (V5).
    pub async fn register(
        &self,
        _ctx: &Context,
        _spec: CommandSpec,
    ) -> Result<EffectHandle, PluginError> {
        todo!("WP-1: register as an effect, most-specific-wins per scope")
    }

    /// Global commands plus the named agent's scoped ones; most-specific-wins on a name clash.
    pub fn list(&self, _scope: Option<&AgentName>) -> Vec<CommandInfo> {
        todo!("WP-1")
    }

    /// The winning spec for this name in this scope.
    pub fn resolve(&self, _name: &CommandName, _scope: Option<&AgentName>) -> Option<CommandSpec> {
        todo!("WP-1")
    }

    /// Resolve, validate args against the schema, run. Appends NO step, starts NO wake, and
    /// emits `commands/dispatched` when it returns.
    pub async fn dispatch(
        &self,
        _inv: Invocation,
        _cx: CommandCx,
    ) -> Result<CommandOutput, CommandError> {
        todo!("WP-1")
    }

    /// The configured command prefix. One character.
    pub fn prefix(&self) -> char {
        todo!("WP-1")
    }
}

/// What `commands/dispatched` carries.
#[derive(Clone, Debug, PartialEq)]
pub struct DispatchRecord {
    pub name: CommandName,
    pub ok: bool,
    pub scope: CommandScope,
    pub at: DateTime<Utc>,
}

/// `commands/dispatched` — EMIT, observability only.
pub struct CommandDispatched;

impl EmitEvent for CommandDispatched {
    const NAME: &'static str = "commands/dispatched";
    type Payload = DispatchRecord;
}

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandsConfig {
    /// The command prefix. One character.
    pub prefix: char,
    /// Levenshtein suggestions on an unknown name.
    pub suggestions: bool,
}

/// The row.
pub struct CommandsPlugin;

#[async_trait::async_trait]
impl Plugin for CommandsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = CommandsConfig;

    fn inject() -> Inject {
        Inject::none()
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-1: provide the `commands` key")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(CommandsPlugin);
