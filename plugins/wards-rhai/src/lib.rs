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
pub mod host;
pub mod invariant;
pub mod testing;
pub mod vocabulary;
pub mod vocabulary_rhai;

use std::path::PathBuf;
use std::sync::Arc;

use std::collections::BTreeMap;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::{AgentName, Ledger, LedgerHandle, Ref, Seq, StepType, TrajId, WakeId};
use bough_plugin_runtime_actions::{RuntimeAction, RuntimeCx, RuntimeLimits};
use chrono::{DateTime, Utc};

pub use vocabulary::{WardFired, WARD_FIRED};

/// How often the reload task looks up from the watch channel to see whether it has been halted.
const RELOAD_POLL: std::time::Duration = std::time::Duration::from_millis(100);
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

/// The floor `validate` enforces on `max_depth`: the host's own prelude (`cx.recent`,
/// `cx.already`) must fit under it, or no ward compiles at all.
pub const MAX_DEPTH_FLOOR: usize = 16;

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
        // The prelude is compiled under the SAME limits as the ward: if a deployment set them
        // below what `cx.recent`/`cx.already` need, that is a config error and it is loud here
        // rather than a mystery in every ward. `MAX_DEPTH_FLOOR` is what `validate` keeps it above.
        let prelude = engine
            .compile(vocabulary_rhai::PRELUDE)
            .map_err(|e| WardError::Compile {
                ward: name.to_string(),
                detail: format!("the host prelude does not fit the configured limits: {e}"),
            })?;
        let ast = engine
            .compile(source)
            .map_err(|e| compile_error(name, &e.to_string()))?;
        if !ast.iter_functions().any(|f| f.name == "on_event") {
            return Err(WardError::NoEntryPoint {
                ward: name.to_string(),
            });
        }
        let has_triggers = ast.iter_functions().any(|f| f.name == "triggers");
        let ast = prelude.merge(&ast);
        let triggers = if has_triggers {
            let mut scope = rhai::Scope::new();
            engine::reset_ops();
            let returned: rhai::Dynamic = engine
                .call_fn(&mut scope, &ast, "triggers", ())
                .map_err(|e| runtime_error(name, &e))?;
            let json = vocabulary_rhai::dynamic_to_json(&returned).map_err(|detail| {
                WardError::BadReturn {
                    ward: name.to_string(),
                    detail,
                }
            })?;
            let list: Vec<String> =
                serde_json::from_value(json).map_err(|e| WardError::BadReturn {
                    ward: name.to_string(),
                    detail: format!("`triggers()` must return a list of step types: {e}"),
                })?;
            list.into_iter().map(StepType::new).collect()
        } else {
            Vec::new()
        };
        Ok(CompiledWard {
            name: name.to_string(),
            triggers,
            ast,
        })
    }

    /// Whether this ward wants to see `kind`. An empty `triggers` means every step type.
    pub fn wants(&self, kind: &StepType) -> bool {
        self.triggers.is_empty() || self.triggers.contains(kind)
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
    let mut scope = rhai::Scope::new();
    engine::reset_ops();
    let returned: rhai::Dynamic = engine
        .call_fn(
            &mut scope,
            &script.ast,
            "on_event",
            (
                vocabulary_rhai::event_map(ev),
                vocabulary_rhai::view_map(cx),
            ),
        )
        .map_err(|e| runtime_error(&script.name, &e))?;
    vocabulary_rhai::actions_of(&script.name, &returned)
}

/// The ops the LAST [`evaluate`] on this thread used, for `ward/fired`.
pub fn last_ops() -> u64 {
    engine::last_ops()
}

/// A compile failure, classified: rhai reports an over-deep expression as a PARSE error, so
/// `max_depth` and `max_ops` are told apart here rather than by the caller reading a string.
fn compile_error(ward: &str, detail: &str) -> WardError {
    let low = detail.to_ascii_lowercase();
    if low.contains("too deep") || low.contains("exceeds maximum") || low.contains("too many") {
        return WardError::TooDeep {
            ward: ward.to_string(),
            max_depth: 0,
        };
    }
    WardError::Compile {
        ward: ward.to_string(),
        detail: detail.to_string(),
    }
}

/// A runtime failure, classified the same way. A ward that runs away is TERMINATED and REPORTED
/// (§7): it is never retried into a loop, so the classification is what the report says.
fn runtime_error(ward: &str, e: &rhai::EvalAltResult) -> WardError {
    match e {
        rhai::EvalAltResult::ErrorTooManyOperations(_) => WardError::TooManyOps {
            ward: ward.to_string(),
            max_ops: engine::last_ops(),
        },
        rhai::EvalAltResult::ErrorStackOverflow(_)
        | rhai::EvalAltResult::ErrorTooManyModules(_) => WardError::TooDeep {
            ward: ward.to_string(),
            max_depth: 0,
        },
        rhai::EvalAltResult::ErrorTerminated(_, _) => WardError::Timeout {
            ward: ward.to_string(),
            ms: 0,
        },
        other => WardError::Runtime {
            ward: ward.to_string(),
            detail: other.to_string(),
        },
    }
}

/// Dry-fire one ward over a list of past events. THE SAME `evaluate` the live path calls, which is
/// what makes `bough wards test` honest: it cannot act, and it cannot drift.
pub fn dry_run(
    script: &CompiledWard,
    events: &[WardEvent],
    cx: &WardView,
    engine: &rhai::Engine,
) -> DryRun {
    let mut fired = Vec::new();
    let mut errors = Vec::new();
    let mut considered = 0usize;
    for ev in events {
        if !script.wants(&ev.kind) {
            continue;
        }
        considered += 1;
        match evaluate(script, ev, cx, engine) {
            Ok(actions) if actions.is_empty() => {}
            Ok(actions) => fired.push((ev.seq, actions)),
            Err(e) => errors.push((ev.seq, e.to_string())),
        }
    }
    DryRun {
        ward: script.name.clone(),
        fired,
        errors,
        considered,
    }
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
    let mut out = format!(
        "ward `{}`: {} events considered, {} would fire, {} errors\n",
        d.ward,
        d.considered,
        d.fired.len(),
        d.errors.len()
    );
    for (seq, actions) in &d.fired {
        out.push_str(&format!("  seq {}:\n", seq.0));
        for a in actions {
            out.push_str(&format!("    would {}\n", describe(a)));
        }
    }
    for (seq, e) in &d.errors {
        out.push_str(&format!("  seq {}: ERROR {e}\n", seq.0));
    }
    out
}

/// One action, in one line, as a person reads it. WOULD, never DID: this renders a dry run.
pub fn describe(a: &RuntimeAction) -> String {
    match a {
        RuntimeAction::Spawn { agent, task, .. } => format!("spawn a worker on `{agent}`: {task}"),
        RuntimeAction::Mark {
            agent, mark, text, ..
        } => format!("mark {mark:?} on `{agent}`: {text}"),
        RuntimeAction::Post { agent, subject, .. } => format!("post `{subject}` into `{agent}`"),
        RuntimeAction::Hint { agent, text } => format!("hint `{agent}`: {text}"),
        RuntimeAction::Schedule { name, in_ms, then } => {
            format!("schedule `{name}` in {in_ms}ms to {}", describe(then))
        }
        RuntimeAction::Act { kind, target, .. } => format!("act {kind} on {target}"),
    }
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
        validate_host(cfg)
    }

    /// Mount one child entry per ward file, and (when `watch`) a notify+debouncer watch that
    /// disposes and remounts EXACTLY the changed child. WP-6.
    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        LedgerHandle(ledger.0.clone())
            .declare_step_types(&ctx, vocabulary::step_types())
            .await?;
        let found = host::scan(&cfg.dir, &cfg.glob)
            .map_err(|e| PluginError::new(entry.clone(), anyhow::Error::from(e)))?;
        let mounted: MountedWards = Arc::new(parking_lot::Mutex::new(BTreeMap::new()));
        for (path, digest) in &found {
            mount_ward(&ctx, &cfg, path, digest, &mounted).await?;
        }
        if cfg.watch {
            watch(&ctx, cfg.clone(), mounted).await?;
        }
        Ok(())
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
        if cfg.digest.is_empty() {
            return Err(ConfigError::Rejected {
                detail: "a ward child carries the sha256 of its file in `digest`".into(),
            });
        }
        validate_host(&cfg.host)
    }

    /// Compile the file, subscribe to `ledger/step`, and on a matching step: `evaluate`, then
    /// `execute_all`, then append ONE `ward/fired`.
    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let name = host::ward_name(&cfg.path);
        let source = std::fs::read_to_string(&cfg.path).map_err(|e| {
            PluginError::new(
                entry.clone(),
                anyhow::anyhow!("{}: {e}", cfg.path.display()),
            )
        })?;
        let engine = Arc::new(engine::build_engine(&cfg.host));
        // A ward that does not compile FAILS ITS OWN ROW: the host reports it and its siblings
        // keep running. It is never retried into a loop (§7).
        let script = Arc::new(
            CompiledWard::compile(&name, &source, &engine)
                .map_err(|e| PluginError::new(entry.clone(), anyhow::Error::from(e)))?,
        );

        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let ledger = LedgerHandle(ledger.0.clone());
        // The step type is declared ONCE, by the host row: it is the host's vocabulary, and a
        // per-child declaration would be N declarations of one type.
        let agents = ctx
            .get::<bough_plugin_agents::Agents>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let agents = bough_plugin_agents::AgentsHandle(agents.0.clone());
        let workers = ctx
            .get::<bough_plugin_workers::Workers>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let workers = bough_plugin_workers::WorkersHandle(workers.0.clone());
        let actions = ctx
            .get::<bough_plugin_actions::Actions>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let actions = bough_plugin_actions::ActionsHandle(actions.0.clone());
        let schedule = ctx
            .get::<bough_plugin_schedule::Schedule>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let schedule = bough_plugin_schedule::ScheduleHandle(schedule.0.clone());

        let acted = Arc::new(parking_lot::Mutex::new(Vec::<Ref>::new()));
        let listen_ctx = ctx.clone();
        ctx.on::<bough_plugin_ledger::LedgerStep, _, _>(move |step| {
            let live = Live {
                ctx: listen_ctx.clone(),
                script: script.clone(),
                engine: engine.clone(),
                ledger: ledger.clone(),
                agents: agents.clone(),
                workers: workers.clone(),
                actions: actions.clone(),
                schedule: schedule.clone(),
                limits: cfg.host.limits.clone(),
                acted: acted.clone(),
            };
            async move {
                if let Err(e) = live.fire(step).await {
                    // REPORTED, NOT RETRIED (§7): a ward that fails is named once per firing and
                    // its siblings and its own next firing are untouched.
                    tracing::warn!(ward = %live.script.name, error = %e, "ward firing failed");
                }
            }
        })
        .await?;
        Ok(())
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
        // An empty `file` is LEGAL: the row is in every headless profile and does nothing unless
        // the `bough wards test` patch layer fills it in.
        if !cfg.since.is_empty() && parse_since(&cfg.since).is_none() {
            return Err(ConfigError::Rejected {
                detail: "`since` must be a seq (`1234`) or a duration (`24h`, `30m`)".into(),
            });
        }
        Ok(())
    }

    /// Dry-fire against past ledger events and print the would-do actions. TOUCHES NO SEAM.
    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        if cfg.file.is_empty() {
            return Ok(());
        }
        let entry = ctx.entry_id().clone();
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let ledger = LedgerHandle(ledger.0.clone());
        let path = PathBuf::from(&cfg.file);
        let source = std::fs::read_to_string(&path).map_err(|e| {
            PluginError::new(entry.clone(), anyhow::anyhow!("{}: {e}", path.display()))
        })?;
        let engine = engine::build_engine(&dry_run_host(&path));
        let script = CompiledWard::compile(&host::ward_name(&path), &source, &engine)
            .map_err(|e| PluginError::new(entry.clone(), anyhow::Error::from(e)))?;
        let steps = ledger
            .0
            .steps(&bough_plugin_ledger::StepQuery {
                order: bough_plugin_ledger::Order::SeqAsc,
                ..Default::default()
            })
            .await
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let cutoff = parse_since(&cfg.since);
        let events: Vec<WardEvent> = steps
            .iter()
            .filter(|s| match cutoff {
                Some(Since::Seq(n)) => s.seq.0 >= n,
                Some(Since::Ago(d)) => s.at >= chrono::Utc::now() - d,
                None => true,
            })
            .map(|s| event_of(s, None))
            .collect();
        let view = WardView {
            ward: script.name.clone(),
            agent_names: Vec::new(),
            now_ms: chrono::Utc::now().timestamp_millis(),
            recent: events.clone(),
            acted: Vec::new(),
        };
        let d = dry_run(&script, &events, &view, &engine);
        let text = match cfg.print {
            Print::Text => render_dry_run(&d),
            Print::Json => serde_json::to_string_pretty(&json_dry_run(&d))
                .unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}")),
        };
        println!("{text}");
        if cfg.exit_when_done {
            if let Some(kernel) = ctx.kernel() {
                kernel.request_exit(0);
            }
        }
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}

/// `--since`, parsed. PURE.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Since {
    Seq(u64),
    Ago(chrono::Duration),
}

/// `1234` ⇒ a seq; `24h` / `30m` / `90s` ⇒ a duration back from now. Anything else is `None`, and
/// `validate` turns that into a loud config error.
pub fn parse_since(s: &str) -> Option<Since> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<u64>() {
        return Some(Since::Seq(n));
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: i64 = num.parse().ok()?;
    match unit {
        "s" => Some(Since::Ago(chrono::Duration::seconds(n))),
        "m" => Some(Since::Ago(chrono::Duration::minutes(n))),
        "h" => Some(Since::Ago(chrono::Duration::hours(n))),
        "d" => Some(Since::Ago(chrono::Duration::days(n))),
        _ => None,
    }
}

/// A dry run as JSON, for `print: json`.
pub fn json_dry_run(d: &DryRun) -> serde_json::Value {
    serde_json::json!({
        "ward": d.ward,
        "considered": d.considered,
        "fired": d.fired.iter().map(|(seq, actions)| serde_json::json!({
            "seq": seq.0, "actions": actions
        })).collect::<Vec<_>>(),
        "errors": d.errors.iter().map(|(seq, e)| serde_json::json!({
            "seq": seq.0, "error": e
        })).collect::<Vec<_>>(),
    })
}

/// The engine limits `bough wards test` dry-fires under. The CLI row has no host config of its
/// own, and a dry run must be at least as strict as the live path, so it uses the DEFAULTS rather
/// than a looser set: a ward that would be terminated live is terminated here too.
pub fn dry_run_host(dir: &std::path::Path) -> WardHostConfig {
    WardHostConfig {
        dir: dir
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf(),
        glob: "*.rhai".into(),
        watch: false,
        debounce_ms: 200,
        max_ops: 100_000,
        max_depth: 32,
        max_string_bytes: 64_000,
        max_array_size: 1_000,
        eval_timeout_ms: 1_000,
        limits: RuntimeLimits::modest(),
    }
}

/// One committed step, as a ward sees it.
pub fn event_of(step: &bough_plugin_ledger::Step, agent: Option<AgentName>) -> WardEvent {
    WardEvent {
        kind: step.kind.clone(),
        seq: step.seq,
        traj: step.traj.clone(),
        agent,
        wake: step.wake.clone(),
        at: step.at,
        body: (*step.body).clone(),
        cites: step.cites.iter().map(|c| c.r#ref.clone()).collect(),
        refs: step.refs.iter().cloned().collect(),
    }
}

/// The shared config checks of the host row and of every child.
fn validate_host(cfg: &WardHostConfig) -> Result<(), ConfigError> {
    if !(MAX_OPS_FLOOR..=MAX_OPS_CEILING).contains(&cfg.max_ops) {
        return Err(ConfigError::Rejected {
            detail: format!("`max_ops` must be between {MAX_OPS_FLOOR} and {MAX_OPS_CEILING}"),
        });
    }
    if cfg.max_depth < MAX_DEPTH_FLOOR {
        return Err(ConfigError::Rejected {
            detail: format!("`max_depth` must be at least {MAX_DEPTH_FLOOR}"),
        });
    }
    if cfg.eval_timeout_ms == 0 {
        return Err(ConfigError::Rejected {
            detail: "`eval_timeout_ms` must be at least 1ms".into(),
        });
    }
    if cfg.max_string_bytes == 0 || cfg.max_array_size == 0 {
        return Err(ConfigError::Rejected {
            detail: "`max_string_bytes` and `max_array_size` must be at least 1".into(),
        });
    }
    if !cfg.glob.starts_with('*') && cfg.glob.contains('*') {
        return Err(ConfigError::Rejected {
            detail: "`glob` must be the `*.ext` shape".into(),
        });
    }
    Ok(())
}

/// Path → the child fiber currently mounted for it, with the digest it was mounted at.
type MountedWards = Arc<parking_lot::Mutex<BTreeMap<PathBuf, (String, bough_kernel::FiberHandle)>>>;

/// Mount ONE ward file as one child entry.
async fn mount_ward(
    ctx: &Context,
    cfg: &WardHostConfig,
    path: &std::path::Path,
    digest: &str,
    mounted: &MountedWards,
) -> Result<(), PluginError> {
    let entry_id = ctx.entry_id().clone();
    let child = bough_kernel::Entry {
        id: bough_kernel::EntryId::new(host::child_id(path)),
        plugin: Some(WARD_PLUGIN_NAME.to_string()),
        config: serde_yaml::to_value(WardConfig {
            path: path.to_path_buf(),
            digest: digest.to_string(),
            host: cfg.clone(),
        })
        .map_err(|e| PluginError::new(entry_id.clone(), e))?,
        disabled: Default::default(),
        isolate: Default::default(),
        inject: Default::default(),
        group: Vec::new(),
        include: None,
    };
    let handle = ctx
        .mount(child)
        .await
        .map_err(|e| PluginError::new(entry_id, e))?;
    mounted
        .lock()
        .insert(path.to_path_buf(), (digest.to_string(), handle));
    Ok(())
}

/// Dispose EXACTLY the child mounted for `path`, and nothing else (P6-D11).
async fn unmount_ward(ctx: &Context, path: &std::path::Path, mounted: &MountedWards) {
    let Some((_, handle)) = mounted.lock().remove(path) else {
        return;
    };
    if let Some(kernel) = ctx.kernel() {
        kernel.runtime().dispose(handle.uid()).await;
    }
}

/// The notify + debouncer watch. One rescan per debounced burst, and the PLAN decides what moves:
/// a file that did not change keeps its fiber, uid and listeners (P6-D11).
async fn watch(
    ctx: &Context,
    cfg: Arc<WardHostConfig>,
    mounted: MountedWards,
) -> Result<(), PluginError> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let debounce = std::time::Duration::from_millis(cfg.debounce_ms);
    let dir = cfg.dir.clone();
    let entry = ctx.entry_id().clone();
    let mut debouncer = notify_debouncer_full::new_debouncer(debounce, None, move |res| {
        if let Ok(_events) = res {
            let _ = tx.send(());
        }
    })
    .map_err(|e| PluginError::new(entry.clone(), anyhow::Error::from(e)))?;
    // A missing wards directory is not a misconfiguration; there is simply nothing to watch yet.
    if dir.exists() {
        debouncer
            .watch(&dir, notify::RecursiveMode::NonRecursive)
            .map_err(|e| PluginError::new(entry.clone(), anyhow::Error::from(e)))?;
    }
    let ctx2 = ctx.clone();
    let handle = ctx.effect_spawn(move |e| async move {
        // The debouncer lives exactly as long as this task, which the fiber owns: unload takes
        // the watch with it and leaves no trace (§0.2).
        let _debouncer = debouncer;
        // A bare `rx.recv().await` never reaches a halt checkpoint, so the fiber cannot settle on
        // unload and the kernel times it out. Poll, and read the halt flag between polls.
        loop {
            if e.is_halted() {
                return Ok(());
            }
            match tokio::time::timeout(RELOAD_POLL, rx.recv()).await {
                Ok(Some(())) => {}
                // The sender went with the watch: nothing more can arrive.
                Ok(None) => return Ok(()),
                Err(_) => continue,
            }
            let found = match host::scan(&cfg.dir, &cfg.glob) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(dir = %cfg.dir.display(), error = %e, "ward rescan failed");
                    continue;
                }
            };
            let current: host::Digests = mounted
                .lock()
                .iter()
                .map(|(p, (d, _))| (p.clone(), d.clone()))
                .collect();
            for change in host::plan_reload(&current, &found) {
                match change {
                    host::Change::Removed(path) => unmount_ward(&ctx2, &path, &mounted).await,
                    host::Change::Added(path) | host::Change::Changed(path) => {
                        unmount_ward(&ctx2, &path, &mounted).await;
                        let digest = found.get(&path).cloned().unwrap_or_default();
                        if let Err(e) = mount_ward(&ctx2, &cfg, &path, &digest, &mounted).await {
                            tracing::warn!(ward = %path.display(), error = %e, "ward remount failed");
                        }
                    }
                }
            }
        }
    });
    drop(handle);
    Ok(())
}

/// Everything one live firing needs, resolved once at mount.
#[derive(Clone)]
struct Live {
    ctx: Context,
    script: Arc<CompiledWard>,
    engine: Arc<rhai::Engine>,
    ledger: LedgerHandle,
    agents: bough_plugin_agents::AgentsHandle,
    workers: bough_plugin_workers::WorkersHandle,
    actions: bough_plugin_actions::ActionsHandle,
    schedule: bough_plugin_schedule::ScheduleHandle,
    limits: RuntimeLimits,
    acted: Arc<parking_lot::Mutex<Vec<Ref>>>,
}

impl Live {
    /// One step: evaluate (PURE), execute through the seams, then append ONE `ward/fired`.
    async fn fire(&self, step: Arc<bough_plugin_ledger::Step>) -> Result<(), anyhow::Error> {
        // A ward never fires on its own journal: that is the loop this host must not have.
        if step.kind.as_str() == vocabulary::WARD_FIRED || !self.script.wants(&step.kind) {
            return Ok(());
        }
        let agent = self
            .agents
            .list()
            .into_iter()
            .find(|a| a.traj() == &step.traj)
            .map(|a| a.name().clone());
        let ev = event_of(&step, agent.clone());
        let view = WardView {
            ward: self.script.name.clone(),
            agent_names: self
                .agents
                .list()
                .iter()
                .map(|a| a.name().to_string())
                .collect(),
            now_ms: step.at.timestamp_millis(),
            recent: self.recent(&step).await,
            acted: self.acted.lock().clone(),
        };
        let started = std::time::Instant::now();
        let actions = evaluate(&self.script, &ev, &view, &self.engine)?;
        let ops = last_ops();
        if actions.is_empty() {
            return Ok(());
        }
        let cx = RuntimeCx {
            ctx: self.ctx.clone(),
            agents: self.agents.clone(),
            ledger: self.ledger.clone(),
            workers: self.workers.clone(),
            actions: self.actions.clone(),
            schedule: self.schedule.clone(),
            source: bough_plugin_runtime_actions::RuntimeSource::Ward(self.script.name.clone()),
            trigger: bough_plugin_runtime_actions::Trigger {
                agent,
                wake: step.wake.clone(),
                step: step.id.clone(),
            },
            at: step.at,
        };
        let outcomes = bough_plugin_runtime_actions::execute_all(&cx, &actions, &self.limits).await;
        self.acted.lock().extend(ev.refs.iter().cloned());
        let body = serde_json::to_value(WardFired {
            ward: self.script.name.clone(),
            on: step.seq,
            actions,
            outcomes: outcomes
                .iter()
                .map(|o| match o {
                    bough_plugin_runtime_actions::ActionOutcome::Did { detail } => {
                        format!("did: {detail}")
                    }
                    bough_plugin_runtime_actions::ActionOutcome::Refused { reason } => {
                        format!("refused: {reason}")
                    }
                })
                .collect(),
            ops,
            ms: started.elapsed().as_millis() as u64,
        })?;
        self.ledger
            .0
            .append(bough_plugin_ledger::Append {
                traj: step.traj.clone(),
                wake: step.wake.clone(),
                kind: StepType::new(vocabulary::WARD_FIRED),
                class: bough_plugin_ledger::Class::Thought,
                body,
                cites: vec![bough_plugin_ledger::Cite {
                    r#ref: Ref::step(&step.id),
                    url: None,
                }],
                at: step.at,
                id: None,
            })
            .await?;
        Ok(())
    }

    /// The bounded peek `cx.recent(kind, n)` reads out of. Fetched HERE so `evaluate` performs no
    /// I/O and stays pure.
    async fn recent(&self, step: &bough_plugin_ledger::Step) -> Vec<WardEvent> {
        let steps = self
            .ledger
            .0
            .steps(&bough_plugin_ledger::StepQuery {
                trajs: vec![step.traj.clone()],
                order: bough_plugin_ledger::Order::SeqDesc,
                limit: Some(RECENT_PEEK),
                ..Default::default()
            })
            .await
            .unwrap_or_default();
        steps.iter().map(|s| event_of(s, None)).collect()
    }
}

/// How many past steps `cx.recent` may see. A PROTOCOL bound, not a tunable: it is what keeps
/// `evaluate` cheap and its input bounded.
pub const RECENT_PEEK: usize = 50;

bough_kernel::register_plugin!(WardHostPlugin);
bough_kernel::register_plugin!(WardPlugin);
bough_kernel::register_plugin!(WardTestPlugin);
