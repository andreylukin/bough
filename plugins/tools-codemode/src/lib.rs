//! Invariant: this row is a `tools` CONSUMER, and the `tools` seam is untouched by it. It
//! registers exactly ONE tool — `run(program)` — and every call a program makes goes through the
//! seam's own pipeline, lands as a ledgered sub-step, and is subject to scope shadowing and
//! `restrict` exactly as a typed call is. Model-visible ⟺ ledgered holds by construction: the
//! only thing the model gets back is console output, and console output is itself a step.

pub mod bind;
pub mod conceal;
pub mod console;
pub mod invariant;
pub mod run;
pub mod surface;
pub mod vocabulary;

use std::collections::BTreeMap;
use std::sync::Arc;

use bough_kernel::{Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_js::Caps;

pub use conceal::{ConcealMode, Mirror};
pub use vocabulary::{ProgramCallBody, ProgramConsoleBody, ProgramErrorBody, ProgramResultBody};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tools-codemode";

/// The ONE API tool. A protocol constant, not config: the TUI, the bench and the surface section
/// all key on it.
pub const RUN_TOOL: &str = "run";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodemodeConfig {
    /// `None` ⇒ the `js` row's `default_caps`.
    #[serde(default)]
    pub caps: Option<Caps>,
    #[serde(default)]
    pub conceal: ConcealMode,
    /// JS name → registered `ToolName`. Ships as `{claim: propose_claim, agent: spawn_worker}`.
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    /// JS namespace → `ToolName` prefix. Ships as `{mcp: "mcp__", act: ""}`.
    #[serde(default)]
    pub namespaces: BTreeMap<String, String>,
    pub max_console_bytes: usize,
    pub max_calls_per_program: u32,
    /// `bash`/`sh` legs must carry 3–5 tags. `false` only for the bench's control arm.
    pub tags_required: bool,
    /// Register the surface documentation as a projection section. `false` for tests that build
    /// the request themselves.
    pub surface_section: bool,
}

/// The Consumer row.
pub struct CodemodePlugin;

#[async_trait::async_trait]
impl Plugin for CodemodePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = CodemodeConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["tools", "js", "ledger", "agents", "projection"])
            .union(&bough_kernel::Inject::optional(["approval"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        if cfg.max_console_bytes == 0 || cfg.max_calls_per_program == 0 {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: "max_console_bytes and max_calls_per_program must be at least 1"
                    .to_string(),
            });
        }
        Ok(())
    }

    /// Registers, as effects: the `run` spec; the concealment (at apply for every live agent and
    /// on `agents::AgentCreated`); the four step types; the `codemode.surface` section; and the
    /// inverse that forgets this fiber's invariant record.
    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let err = |e: bough_kernel::KernelError| PluginError::new(entry.clone(), e);
        let tools = ctx.get::<bough_plugin_tools::Tools>().map_err(err)?;
        let js = ctx.get::<bough_plugin_js::Js>().map_err(err)?;
        let ledger = ctx.get::<bough_plugin_ledger::Ledger>().map_err(err)?;
        let agents = ctx.get::<bough_plugin_agents::Agents>().map_err(err)?;
        let projection = ctx
            .get::<bough_plugin_projection::Projection>()
            .map_err(err)?;

        // Fail loud (§0.2): a code-mode row with no JS engine is a harness that can accept a
        // program and never run one. That is a boot failure, not a runtime surprise.
        if js.engine().is_none() {
            return Err(PluginError::new(
                entry.clone(),
                anyhow::anyhow!(
                    "`tools-codemode` needs a `js` engine provider (mount the `js.quickjs` row); \
                     the `js` seam has none"
                ),
            ));
        }

        // The `program/*` vocabulary is declared for the life of the BINARY, not the life of the
        // row — deliberately NOT through `ledger.declare_step_types`, whose registration is an
        // effect that unwinds on unload.
        //
        // A step type describes bytes that are ALREADY ON DISK. A trajectory that once ran a
        // program keeps its `program/call`/`program/result`/`program/console` rows forever, and a
        // rebuild of that trajectory dies at the first one whose type this binary does not know
        // ("unknown to this binary and not ignorable"). Unwinding the declaration therefore made
        // the consumer swap ONE-WAY on any chain that had used it: disabling `tools.codemode`
        // killed the next wake. `docs/codemode-merge-notes.md` §10 recorded it as a defect; this
        // is the fix. The registration is idempotent (`StepTypeMap::register` takes a reference
        // on a byte-identical redeclaration), so a remount is not a duplicate.
        for def in vocabulary::step_types() {
            // Dropping the token spends it without ever unregistering — what `builtin_step_types`
            // does for the ledger's own sixteen.
            drop(
                ledger
                    .0
                    .register_step_type(def)
                    .map_err(|e| PluginError::new(entry.clone(), anyhow::anyhow!(e)))?,
            );
        }

        let conceal = Arc::new(conceal::Concealment::new(cfg.conceal));
        let run = Arc::new(run::Run {
            cfg: cfg.clone(),
            ctx: ctx.clone(),
            fiber: ctx.fiber_uid(),
            js: (*js).clone(),
            tools: (*tools).clone(),
            ledger: (*ledger).clone(),
            conceal: conceal.clone(),
        });
        tools.register(&ctx, run::spec(run)).await?;

        // Conceal for every agent alive now, and for every agent born later. A lane is created at
        // runtime, so the retry is the rule and the loop below is the catch-up.
        for agent in agents.list() {
            conceal.install(&ctx, &tools, agent.name()).await?;
        }
        let ctx2 = ctx.clone();
        let tools2 = (*tools).clone();
        let conceal2 = conceal.clone();
        ctx.on::<bough_plugin_agents::AgentCreated, _, _>(move |agent| {
            let ctx = ctx2.clone();
            let tools = tools2.clone();
            let conceal = conceal2.clone();
            async move {
                let name = agent.name().clone();
                if let Err(e) = conceal.install(&ctx, &tools, &name).await {
                    tracing::error!(agent = %name, error = %e, "tools-codemode: concealing failed");
                }
            }
        })
        .await?;

        if cfg.surface_section {
            let source: Arc<dyn surface::SurfaceSource> = Arc::new(RegistrySource {
                cfg: cfg.clone(),
                tools: (*tools).clone(),
                conceal: conceal.clone(),
            });
            projection
                .section(&ctx, surface::Surface::spec(source))
                .await?;
        }

        // The recorded stream this crate's invariant reads is per fiber LIFE (§0.3).
        let mine = ctx.fiber_uid();
        ctx.effect(move |e| async move {
            e.defer_sync(move || invariant::forget(mine));
            Ok(())
        })
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::every_program_call_is_ledgered()]
    }
}

/// The section's roster, read from the LIVE registry through the same derivation the sandbox
/// injects from — so the documented list and the injected globals cannot drift.
struct RegistrySource {
    cfg: Arc<CodemodeConfig>,
    tools: bough_plugin_tools::ToolsHandle,
    conceal: Arc<conceal::Concealment>,
}

impl surface::SurfaceSource for RegistrySource {
    fn bindings(&self, agent: &bough_plugin_ledger::AgentName) -> Vec<bind::Binding> {
        // Under `Mirror` the real handle answers `run` and nothing else, so the roster comes from
        // the unrestricted view cached when the restriction went on and refreshed at every
        // program. Under `None` nothing is hidden and the live read is the truth.
        let specs = match self.conceal.cached_specs(agent) {
            Some(specs) => specs,
            None => conceal::visible_specs(&self.tools, agent),
        };
        bind::bindings(&specs, &self.cfg.aliases, &self.cfg.namespaces).unwrap_or_default()
    }

    fn sees_run(&self, agent: &bough_plugin_ledger::AgentName) -> bool {
        self.conceal.sees_run(&self.tools, agent)
    }
}

bough_kernel::register_plugin!(CodemodePlugin);
