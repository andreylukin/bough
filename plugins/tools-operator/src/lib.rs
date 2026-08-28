//! Invariant: these eight are ORDINARY tools. Nothing here reaches around the `tools` seam — each
//! is registered through `ToolsHandle::register` and guarded by the same pipeline — and the row
//! is mounted for BOTH consumers, so the bench compares SURFACES and not tool inventories.

pub mod bg;
pub mod clock;
pub mod files;
pub mod inbox;
pub mod invariant;
pub mod ledger_read;
pub mod schedule;
pub mod sh;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::AgentsHandle;
use bough_plugin_ledger::LedgerHandle;
use bough_plugin_tools::{RenderIntent, ToolName, ToolScope, ToolSpec};

pub use clock::{Clock, SystemClock};
pub use schedule::{ScheduleFiredBody, ScheduleId, ScheduleIntentBody};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tools-operator";

/// What this row registers. `view`/`patch`/`write` are the file verbs; `sh`, `bg`, `ledger_read`,
/// `inbox` and `schedule` are the five the sandbox surface sugars.
///
/// MERGE: `sh` — the CONCURRENT shell — arrived here at the merge. `surface/shell.md` had taught
/// it since the phase was written and no row registered it (`docs/codemode-merge-notes.md` §9,
/// "Still open"), so the sandbox advertised a function that did not exist. `tools-baseline` keeps
/// the ONE serial shell (`bash`); the concurrent and background shells are this row's.
pub const TOOL_NAMES: [&str; 8] = [
    "view",
    "patch",
    "write",
    "sh",
    "bg",
    "ledger_read",
    "inbox",
    "schedule",
];

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperatorConfig {
    pub max_view_bytes: usize,
    pub max_files_per_patch: usize,
    pub bg_log_dir: PathBuf,
    pub bg_max: usize,
    pub bg_poll_ms: u64,
    pub ledger_page: usize,
    pub schedule_max_horizon_days: u32,
    pub schedule_tick_ms: u64,
    /// How many legs one `sh` call may run at once.
    pub sh_max_legs: usize,
    /// The per-leg wall clock. `bash_timeout_ms`'s opposite number, and its own field because a
    /// deployment that shortens one has no reason to shorten the other.
    pub sh_timeout_ms: u64,
    /// The inclusive tag count every `sh` leg must carry. The rule is the TOOL's, not a surface's:
    /// an untagged command is one no future session can find, whoever issued it.
    pub sh_tags_min: usize,
    pub sh_tags_max: usize,
}

/// The Consumer row.
pub struct OperatorPlugin;

#[async_trait::async_trait]
impl Plugin for OperatorPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = OperatorConfig;

    /// Only what `apply` actually reads. A declared key is a RELOAD TRIGGER: naming `mail` or
    /// `schedule` here would remount this row the moment a provider for either appeared, for no
    /// gain, since nothing in `apply` looks either up.
    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["tools", "ledger", "workspace"])
            .union(&bough_kernel::Inject::optional(["agents"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        if cfg.max_view_bytes == 0
            || cfg.max_files_per_patch == 0
            || cfg.bg_max == 0
            || cfg.bg_poll_ms == 0
            || cfg.ledger_page == 0
            || cfg.schedule_tick_ms == 0
            || cfg.sh_max_legs == 0
            || cfg.sh_timeout_ms == 0
            || cfg.sh_tags_min == 0
        {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: "every bound must be at least 1".to_string(),
            });
        }
        if cfg.sh_tags_min > cfg.sh_tags_max {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: "sh_tags_min must not exceed sh_tags_max".to_string(),
            });
        }
        Ok(())
    }

    /// Registers all seven specs as effects, declares the two `schedule/*` step types, starts the
    /// due-watcher, and kills every live `bg` job on disposal.
    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let fail = |e: bough_kernel::KernelError| PluginError::new(entry.clone(), e);
        let tools = (*ctx.get::<bough_plugin_tools::Tools>().map_err(fail)?).clone();
        let ledger = (*ctx.get::<bough_plugin_ledger::Ledger>().map_err(fail)?).clone();
        let root = (*ctx.get::<bough_plugin_tools::Workspace>().map_err(fail)?).clone();
        let agents = ctx
            .try_get::<bough_plugin_agents::Agents>()
            .map_err(fail)?
            .map(|a| (*a).clone());

        // The two step types are declared BEFORE the `schedule` tool can be called: an append of a
        // type the ledger does not know is refused, and a tool that could only fail is worse than
        // an absent one.
        ledger
            .declare_step_types(&ctx, schedule::step_types())
            .await?;

        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let seen = Arc::new(files::SeenFiles::default());
        let jobs = bg::BgJobs::new(cfg.clone(), root.path().to_path_buf());

        // §0.2: unwind leaves no orphan. The kill is an INVERSE of the row, so a disable, a
        // reload and a boot failure all reach it — a `Drop` impl on the registry would not,
        // because a live `Arc` in a tool spec outlives the row that made it.
        {
            let jobs = jobs.clone();
            let fiber = ctx.fiber_uid();
            ctx.effect(move |ectx| async move {
                ectx.defer_sync(move || {
                    jobs.kill_all();
                    invariant::forget(fiber);
                });
                Ok(())
            })
            .await?;
        }

        for spec in files::specs(cfg.clone(), root.clone(), seen) {
            tools.register(&ctx, spec).await?;
        }
        for spec in specs(
            cfg.clone(),
            clock.clone(),
            ledger.clone(),
            agents.clone(),
            jobs,
            root.path().to_path_buf(),
        ) {
            tools.register(&ctx, spec).await?;
        }

        // The watcher needs a live registry to wake: with `agents` absent an intent is still
        // recorded and still fires when the row is remounted with one, which is the whole reason
        // the intent is a ledger step.
        if let Some(agents) = agents {
            let watcher = schedule::Watcher {
                cfg,
                clock,
                ledger,
                agents,
                fiber: ctx.fiber_uid(),
            };
            ctx.effect_spawn(move |ectx| async move { schedule::watch(ectx, watcher).await });
        }
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::every_fire_names_a_live_intent()]
    }
}

fn schema(v: serde_json::Value) -> schemars::Schema {
    schemars::Schema::try_from(v).expect("an operator tool's input schema is an object")
}

/// The five specs this module owns; `files::specs` supplies the other three.
///
/// The plan's signature took only `(cfg)`: every one of the four needs an injected dependency the
/// config cannot name (a clock, the ledger, the live registry, the job table), so they are
/// arguments — see `docs/codemode-merge-notes.md`.
pub fn specs(
    cfg: Arc<OperatorConfig>,
    clock: Arc<dyn Clock>,
    ledger: LedgerHandle,
    agents: Option<AgentsHandle>,
    jobs: Arc<bg::BgJobs>,
    root: PathBuf,
) -> Vec<ToolSpec> {
    let string = serde_json::json!({ "type": "string" });
    vec![
        ToolSpec {
            name: ToolName::new("sh"),
            description: "Run several shell commands CONCURRENTLY in the task tree and return \
                          `[{code, out}, …]` in leg order. A non-zero exit is data, never a \
                          failure. Every leg needs 3-5 short lowercase `tags` naming the tool, \
                          the intent and the subject: they index the command in the \
                          cross-session history."
                .into(),
            input_schema: schema(serde_json::json!({
                "type": "object",
                "properties": { "legs": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "cmd": string,
                            "tags": {
                                "type": "array",
                                "items": { "type": "string" },
                                "minItems": 3,
                                "maxItems": 5
                            }
                        },
                        "required": ["cmd", "tags"]
                    }
                }},
                "required": ["legs"]
            })),
            render: RenderIntent::Terminal,
            scope: ToolScope::Global,
            tool: Arc::new(sh::Sh {
                cfg: cfg.clone(),
                root,
            }),
        },
        ToolSpec {
            name: ToolName::new("bg"),
            description: "Run a shell command in the background, read its output, or kill it. \
                          `{op: start, name, cmd}` | `{op: output, id}` | `{op: kill, id}`."
                .into(),
            input_schema: schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "op": { "type": "string", "enum": ["start", "output", "kill"] },
                    "name": string, "cmd": string, "id": string
                },
                "required": ["op"]
            })),
            render: RenderIntent::Terminal,
            scope: ToolScope::Global,
            tool: Arc::new(bg::Bg {
                cfg: cfg.clone(),
                jobs,
            }),
        },
        ToolSpec {
            name: ToolName::new("ledger_read"),
            description: "Drill into the ledger: `{op: search, q}` | `{op: steps, from, to}` | \
                          `{op: tail, n}`. Results cite the steps they came from."
                .into(),
            input_schema: schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "op": { "type": "string", "enum": ["search", "steps", "tail"] },
                    "q": string,
                    "range": string,
                    "from": { "type": "integer" }, "to": { "type": "integer" },
                    "n": { "type": "integer" }, "limit": { "type": "integer" }
                },
                "required": ["op"]
            })),
            render: RenderIntent::Generic,
            scope: ToolScope::Global,
            tool: Arc::new(ledger_read::LedgerRead {
                cfg: cfg.clone(),
                ledger: ledger.clone(),
            }),
        },
        ToolSpec {
            name: ToolName::new("inbox"),
            description: "The mail delivered to this agent that no wake has consumed yet.".into(),
            input_schema: schema(serde_json::json!({ "type": "object", "properties": {} })),
            render: RenderIntent::Generic,
            scope: ToolScope::Global,
            tool: Arc::new(inbox::Inbox {
                ledger: ledger.clone(),
                agents: agents.clone(),
            }),
        },
        ToolSpec {
            name: ToolName::new("schedule"),
            description: "Wake yourself later with an intent. `at` is an RFC 3339 instant or a \
                          `+5m` / `+2h` / `+1d` offset."
                .into(),
            input_schema: schema(serde_json::json!({
                "type": "object",
                "properties": { "at": string, "intent": string },
                "required": ["at", "intent"]
            })),
            render: RenderIntent::Generic,
            scope: ToolScope::Global,
            tool: Arc::new(schedule::Schedule {
                cfg,
                clock,
                ledger,
                agents,
            }),
        },
    ]
}

bough_kernel::register_plugin!(OperatorPlugin);

/// A branded id is a plain string in a body schema; `brand_id!` lives in `bough-util`, which has
/// no `schemars` dependency, so the impls are written here — the same shape every id in the tree
/// already has.
macro_rules! id_json_schema {
    ($($t:ty),* $(,)?) => {$(
        impl schemars::JsonSchema for $t {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(stringify!($t))
            }
            fn json_schema(_g: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({ "type": "string" })
            }
        }
    )*};
}

id_json_schema!(schedule::ScheduleId, bg::BgId);
