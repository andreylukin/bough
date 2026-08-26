//! Invariant: a ward is PURE. [`evaluate`] touches NO seam: it takes an event and a read-only view
//! and returns a list of [`RuntimeAction`]s. The host is what executes them, through
//! `runtime_actions::execute_all`. That is why `bough wards test` cannot act by accident and cannot
//! drift from live behaviour — both paths call the SAME `evaluate`.
//!
//! The sandbox is [`engine::build_engine`]: a raw engine with arithmetic/array/map packages only,
//! no filesystem, no process, no network, no `print` sink beyond a captured string, and `eval`
//! DISABLED explicitly (rhai enables it by default — §13 names this).
//!
//! P6-D11: hot reload is dispose-then-mount of exactly ONE child fiber, verified by comparing every
//! other fiber uid in the tree.

pub mod engine;
pub mod invariant;
pub mod vocabulary;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::{AgentName, Ref, Seq, StepType, TrajId, WakeId};
use bough_plugin_runtime_actions::{RuntimeAction, RuntimeLimits};
use chrono::{DateTime, Utc};

pub use vocabulary::{WardFired, WARD_FIRED};

/// The catalog name of the host row.
pub const PLUGIN_NAME: &str = "wards-rhai";
/// The catalog name of the per-file CHILD row.
pub const WARD_PLUGIN_NAME: &str = "ward";
/// The catalog name of the CLI row.
pub const WARD_TEST_PLUGIN_NAME: &str = "ward-test";

/// The host row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WardHostConfig {
    /// `~/.bough/wards`.
    pub dir: PathBuf,
    /// `"*.rhai"`.
    pub glob: String,
    pub watch: bool,
    pub debounce_ms: u64,
    /// Engine limits (P6-D10). WHICH limits are set is code — all five, always, plus `eval`
    /// disabled, plus a raw engine with no I/O packages. Their VALUES are config, bounded by
    /// `Plugin::validate`: a `max_ops` of zero or of a billion is refused at load.
    pub max_ops: u64,
    /// Expression + function-call depth.
    pub max_depth: usize,
    pub max_string_bytes: usize,
    pub max_array_size: usize,
    pub eval_timeout_ms: u64,
    pub limits: RuntimeLimits,
}

/// The floor and ceiling `validate` enforces on `max_ops` (P6-D10).
pub const MAX_OPS_FLOOR: u64 = 1;
/// See [`MAX_OPS_FLOOR`].
pub const MAX_OPS_CEILING: u64 = 5_000_000;

/// One ward file's child config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WardConfig {
    pub path: PathBuf,
    /// sha256 of the file. A change HERE is what reloads exactly this one child (§0.3 per-field
    /// reconcile).
    pub digest: String,
    pub host: WardHostConfig,
}

/// A compiled ward: its name, its declared triggers and its AST.
pub struct CompiledWard {
    pub name: String,
    /// From the script's optional `triggers()`. Empty ⇒ every step type.
    pub triggers: Vec<StepType>,
    ast: rhai::AST,
}

impl CompiledWard {
    /// Compile a ward file and read its `triggers()`. PURE apart from the compile. WP-6.
    pub fn compile(
        name: &str,
        source: &str,
        engine: &rhai::Engine,
    ) -> Result<CompiledWard, WardError> {
        let _ = (name, source, engine);
        todo!("WP-6")
    }

    /// The compiled AST, for the evaluator.
    pub fn ast(&self) -> &rhai::AST {
        &self.ast
    }
}

/// One ledger step, as a ward sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct WardEvent {
    pub kind: StepType,
    pub seq: Seq,
    pub traj: TrajId,
    pub agent: Option<AgentName>,
    pub wake: WakeId,
    pub at: DateTime<Utc>,
    pub body: serde_json::Value,
    pub cites: Vec<Ref>,
    pub refs: Vec<Ref>,
}

/// The READ-ONLY context a ward is handed. Everything on it was resolved before the call, so
/// [`evaluate`] performs no I/O.
#[derive(Clone, Debug, PartialEq)]
pub struct WardView {
    pub ward: String,
    pub agent_names: Vec<String>,
    pub now_ms: i64,
    /// Pre-fetched, bounded peeks the script's `recent(kind, n)` reads out of.
    pub recent: Vec<WardEvent>,
    /// Refs this ward has already acted on; the script's `already(ref)` reads it.
    pub acted: Vec<Ref>,
}

/// The whole of a ward, as both the live path and the dry-fire path see it. PURE. WP-6.
pub fn evaluate(
    script: &CompiledWard,
    ev: &WardEvent,
    cx: &WardView,
    engine: &rhai::Engine,
) -> Result<Vec<RuntimeAction>, WardError> {
    let _ = (script, ev, cx, engine);
    todo!("WP-6: call `on_event(ev, cx)`; a returned non-array is a named error")
}

/// A dry run's whole output. `bough wards test` prints this; a test asserts on it.
#[derive(Clone, Debug, PartialEq)]
pub struct DryRun {
    pub ward: String,
    pub fired: Vec<(Seq, Vec<RuntimeAction>)>,
    pub errors: Vec<(Seq, String)>,
    pub considered: usize,
}

/// PURE: the text `bough wards test` prints. WP-6.
pub fn render_dry_run(d: &DryRun) -> String {
    let _ = d;
    todo!("WP-6")
}

/// What a ward goes wrong as. A ward is REPORTED and not retried into a loop (§7).
#[derive(Debug, thiserror::Error)]
pub enum WardError {
    #[error("ward `{ward}` failed to compile: {detail}")]
    Compile { ward: String, detail: String },
    #[error("ward `{ward}` has no `on_event` function")]
    NoEntryPoint { ward: String },
    #[error("ward `{ward}` exceeded its operation limit ({max_ops} ops)")]
    TooManyOps { ward: String, max_ops: u64 },
    #[error("ward `{ward}` exceeded its depth limit ({max_depth})")]
    TooDeep { ward: String, max_depth: usize },
    #[error("ward `{ward}` timed out after {ms}ms")]
    Timeout { ward: String, ms: u64 },
    #[error("ward `{ward}` returned something that is not a list of actions: {detail}")]
    BadReturn { ward: String, detail: String },
    #[error("ward `{ward}`: {detail}")]
    Runtime { ward: String, detail: String },
}

/// The host row.
pub struct WardHostPlugin;

#[async_trait::async_trait]
impl Plugin for WardHostPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = WardHostConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["ledger", "workers", "actions", "agents", "schedule"])
            .union(&bough_kernel::Inject::optional(["commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-6: `max_ops` within [MAX_OPS_FLOOR, MAX_OPS_CEILING], non-zero depth/timeout")
    }

    /// Mount one child entry per ward file, and (when `watch`) a notify+debouncer watch that
    /// disposes and remounts EXACTLY the changed child. WP-6.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-6")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

/// One ward file's child row.
pub struct WardPlugin;

#[async_trait::async_trait]
impl Plugin for WardPlugin {
    const NAME: &'static str = WARD_PLUGIN_NAME;
    type Config = WardConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["ledger", "workers", "actions", "agents", "schedule"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-6")
    }

    /// Compile the file, subscribe to `ledger/step`, and on a matching step: `evaluate`, then
    /// `execute_all`, then append ONE `ward/fired`. WP-6.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-6")
    }

    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}

/// How `bough wards test` prints.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Print {
    Text,
    Json,
}

/// The CLI row's config. An empty `file` ⇒ the row mounts and does nothing.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WardTestConfig {
    #[serde(default)]
    pub file: String,
    /// A seq or a duration (`24h`). Empty ⇒ the whole tail the ledger will give.
    #[serde(default)]
    pub since: String,
    pub print: Print,
    pub exit_when_done: bool,
}

/// The CLI row.
pub struct WardTestPlugin;

#[async_trait::async_trait]
impl Plugin for WardTestPlugin {
    const NAME: &'static str = WARD_TEST_PLUGIN_NAME;
    type Config = WardTestConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["ledger"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-6: an empty `file` is legal; `since` must parse when present")
    }

    /// Dry-fire against past ledger events and print the would-do actions. TOUCHES NO SEAM. WP-6.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-6")
    }

    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}

bough_kernel::register_plugin!(WardHostPlugin);
bough_kernel::register_plugin!(WardPlugin);
bough_kernel::register_plugin!(WardTestPlugin);
