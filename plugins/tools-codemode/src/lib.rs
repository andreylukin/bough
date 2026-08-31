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

use std::collections::{BTreeMap, BTreeSet};
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
    /// JS name → registered `ToolName`. Ships as `{agent: spawn_worker}`.
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    /// JS namespace → `ToolName` prefix. Ships as `{mcp: "mcp__", act: ""}`.
    #[serde(default)]
    pub namespaces: BTreeMap<String, String>,
    /// Registered tools that are NOT injected and NOT documented under code mode. The phase
    /// brief's "drop as separate functions": `bash`/`view`/`patch` cover what `read_file`,
    /// `glob`, `grep` and `edit_file` do, and a second spelling of the same verb is surface the
    /// model has to choose between. Visibility only — the tool stays registered and callable by a
    /// typed-tools agent.
    #[serde(default)]
    pub hide: BTreeSet<String>,
    /// The registered names this Consumer treats as SHELL tools: they take code mode's tag
    /// argument and are subject to the tag rule. A Provider that spells its shell `shell` names
    /// it here; nothing in the code knows `bash`.
    #[serde(default = "default_shell_tools")]
    pub shell_tools: BTreeSet<String>,
    /// Shell tools whose textual output is what the JS call RETURNS, even when the tool also
    /// produced a `value` — `surface/shell.md` promises `bash()` returns a string.
    #[serde(default = "default_shell_tools")]
    pub shell_content_result: BTreeSet<String>,
    /// The inclusive tag count a shell call must carry when `tags_required`.
    #[serde(default = "default_tags_min")]
    pub tags_min: usize,
    #[serde(default = "default_tags_max")]
    pub tags_max: usize,
    /// The deadline an INNER call gets. `None` ⇒ the program's wall clock, which is the only
    /// bound an inner call can legitimately outlive nothing under. Set it to `tools`'
    /// `default_deadline_ms` to make a typed call and a program call answer identically; the seam
    /// does not expose its own value to read (see `docs/codemode-merge-notes.md`).
    #[serde(default)]
    pub inner_deadline_ms: Option<u64>,
    /// How many concurrency-safe INNER calls may dispatch at once — code mode's spelling of
    /// `tools.max_parallel`, which a single-call `execute_under` per host call cannot apply.
    #[serde(default = "default_max_parallel_calls")]
    pub max_parallel_calls: usize,
    pub max_console_bytes: usize,
    pub max_calls_per_program: u32,
    /// Shell legs must carry `tags_min`–`tags_max` tags. `false` only for the bench's control arm.
    pub tags_required: bool,
    /// Register the surface documentation as a projection section. `false` for tests that build
    /// the request themselves.
    pub surface_section: bool,
}

fn default_shell_tools() -> BTreeSet<String> {
    BTreeSet::from(["bash".to_string()])
}
fn default_tags_min() -> usize {
    3
}
fn default_tags_max() -> usize {
    5
}
fn default_max_parallel_calls() -> usize {
    8
}

impl CodemodeConfig {
    /// What the Consumer knows about shell tools, as one value.
    pub fn shell_rules(&self) -> bind::ShellRules {
        bind::ShellRules {
            tools: self.shell_tools.clone(),
            content_result: self.shell_content_result.clone(),
            tags_min: self.tags_min,
            tags_max: self.tags_max,
            tags_required: self.tags_required,
        }
    }

    /// THE binding derivation. The injected globals and the documented roster both come from
    /// here, so a hidden tool cannot be documented, and a documented one cannot be missing.
    pub fn surface_bindings(
        &self,
        specs: &[bough_plugin_tools::ToolSpec],
    ) -> Result<Vec<bind::Binding>, bind::BindError> {
        bind::bindings_hiding(specs, &self.aliases, &self.namespaces, &self.hide)
    }
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

    /// §0.2: everything self-contained fails HERE, at load.
    ///
    /// `aliases` and `namespaces` decide the whole injected surface and their legality needs
    /// nothing but themselves, so they are checked here rather than degrading a round at a time.
    /// `conceal: seam` names a seam call that does not exist on this branch, so it is rejected
    /// unless the `seam-conceal` feature is on: it used to boot green on any tree whose lanes are
    /// created later and leave every agent unconcealed.
    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        let reject = |detail: String| bough_kernel::ConfigError::Rejected { detail };
        if cfg.max_console_bytes == 0 || cfg.max_calls_per_program == 0 {
            return Err(reject(
                "max_console_bytes and max_calls_per_program must be at least 1".to_string(),
            ));
        }
        if cfg.max_parallel_calls == 0 {
            return Err(reject("max_parallel_calls must be at least 1".to_string()));
        }
        if cfg.tags_min == 0 || cfg.tags_min > cfg.tags_max {
            return Err(reject(format!(
                "tags_min ({}) must be at least 1 and no greater than tags_max ({})",
                cfg.tags_min, cfg.tags_max
            )));
        }
        if !cfg.shell_content_result.is_subset(&cfg.shell_tools) {
            return Err(reject(
                "every shell_content_result name must also be a shell_tools name".to_string(),
            ));
        }
        bind::validate_names(&cfg.aliases, &cfg.namespaces)
            .map_err(|e| reject(format!("the injected surface cannot be built: {e}")))?;
        #[cfg(not(feature = "seam-conceal"))]
        if cfg.conceal == ConcealMode::Seam {
            return Err(reject(
                "conceal `seam` needs `ToolsHandle::conceal`, which does not exist on this branch \
                 (build with the `seam-conceal` feature once it lands); use `mirror`"
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// Registers, as effects: the `run` spec; the concealment (at apply for every live agent and
    /// at the `agent/wake-request` waterfall for every agent born later); the four step types; the `codemode.surface` section; and the
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

        // The `program/*` vocabulary. `declare_step_types` registers for the life of the BINARY
        // (see its doc comment): a step type describes bytes that are ALREADY ON DISK, and a
        // trajectory that once ran a program keeps its `program/*` rows forever. When the
        // declaration unwound with the row, disabling `tools.codemode` killed the NEXT WAKE on
        // any chain that had used it — `docs/codemode-merge-notes.md` §10. The rule was moved
        // into `plugins/ledger` at the merge, so every plugin's vocabulary now behaves this way
        // and this row does nothing special.
        ledger
            .declare_step_types(&ctx, vocabulary::step_types())
            .await?;

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

        // Conceal every agent alive NOW (a remount over a running tree); every agent born later
        // is concealed by the admission waterfall below, before its first wake exists.
        for agent in agents.list() {
            conceal.install(&ctx, &tools, agent.name()).await?;
        }
        // The EAGER path: conceal as soon as an agent exists, so a reader of `tools.schemas()`
        // between creation and the first wake already sees the code-mode surface. It is only a
        // warm-up — `agent/created` is an EMIT, dispatched fire-and-forget — so a failure here is
        // logged and left to the waterfall below, which is the one that cannot be raced.
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

        // THE GUARANTEE, and why the listener above is only a warm-up:
        // an agent created and woken in the same breath could build its FIRST request with the whole typed tool list next to `run` —
        // while being handed a surface section that says "there are no other tools". The
        // admission waterfall is AWAITED by every loop Provider immediately before the wake
        // exists, so concealing here happens strictly before any request is built. A failure
        // DEFERS the wake instead of running it unconcealed: §0.2's "never silently skip".
        let ctx3 = ctx.clone();
        let tools3 = (*tools).clone();
        let conceal3 = conceal.clone();
        if false {
            ctx.on_waterfall::<bough_plugin_agents::AgentWakeRequest, _, _>(move |mut v, next| {
                let ctx = ctx3.clone();
                let tools = tools3.clone();
                let conceal = conceal3.clone();
                async move {
                    if matches!(v.decision, bough_plugin_agents::Admit::Defer { .. }) {
                        return next.run(v).await;
                    }
                    if let Err(e) = conceal.install(&ctx, &tools, &v.agent).await {
                        v.decision = bough_plugin_agents::Admit::Defer {
                            by: PLUGIN_NAME,
                            reason: format!(
                                "the code-mode tool surface could not be concealed: {e}"
                            ),
                        };
                        return v;
                    }
                    next.run(v).await
                }
            })
            .await?;
        }

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
        // `validate` rejected an illegal `aliases`/`namespaces` at LOAD, so this cannot fail for
        // a misconfiguration; an error here would be a registry that changed under us, and the
        // honest answer is then the empty roster rather than a section that documents a guess.
        self.cfg.surface_bindings(&specs).unwrap_or_default()
    }

    fn sees_run(&self, agent: &bough_plugin_ledger::AgentName) -> bool {
        self.conceal.sees_run(&self.tools, agent)
    }
}

bough_kernel::register_plugin!(CodemodePlugin);

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> CodemodeConfig {
        CodemodeConfig {
            caps: None,
            conceal: ConcealMode::Mirror,
            aliases: BTreeMap::new(),
            namespaces: BTreeMap::new(),
            hide: BTreeSet::new(),
            shell_tools: default_shell_tools(),
            shell_content_result: default_shell_tools(),
            tags_min: 3,
            tags_max: 5,
            inner_deadline_ms: None,
            max_parallel_calls: 8,
            max_console_bytes: 4096,
            max_calls_per_program: 16,
            tags_required: true,
            surface_section: true,
        }
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    /// §0.2: everything self-contained fails at LOAD. `aliases`/`namespaces` decide the whole
    /// injected surface; before this they were checked nowhere, so a one-character typo in a
    /// bundle patch booted green and then degraded every round in two directions at once (an
    /// empty documented roster, and a ToolFailure per call).
    #[test]
    fn an_illegal_alias_map_is_a_boot_failure() {
        assert!(CodemodePlugin::validate(&cfg()).is_ok());

        let mut bad = cfg();
        bad.aliases = map(&[("ledger-search", "ledger_read")]);
        let e = CodemodePlugin::validate(&bad).expect_err("a non-identifier alias must be caught");
        assert!(format!("{e:?}").contains("ledger-search"), "{e:?}");

        let mut bad = cfg();
        bad.namespaces = map(&[("act", "")]);
        CodemodePlugin::validate(&bad).expect_err("an empty namespace prefix must be caught");
    }

    /// `conceal: seam` names `ToolsHandle::conceal`, which does not exist on this branch. It used
    /// to deserialise happily and fail only inside `install` — which `apply` reaches ONLY for
    /// agents that already exist, so on a tree whose lanes are created later (the normal case)
    /// boot succeeded and every agent ran with its whole typed tool list in the prompt.
    #[test]
    #[cfg(not(feature = "seam-conceal"))]
    fn conceal_seam_is_rejected_at_load_while_the_seam_call_is_missing() {
        let mut bad = cfg();
        bad.conceal = ConcealMode::Seam;
        let e = CodemodePlugin::validate(&bad).expect_err("`seam` must not boot");
        assert!(format!("{e:?}").contains("seam"), "{e:?}");
    }

    /// The remaining self-contained numbers.
    #[test]
    fn the_tunables_are_bounded_at_load() {
        let mut bad = cfg();
        bad.max_parallel_calls = 0;
        CodemodePlugin::validate(&bad).expect_err("a zero dispatch limit is not a limit");

        let mut bad = cfg();
        bad.tags_min = 6;
        CodemodePlugin::validate(&bad).expect_err("an empty tag range refuses every shell call");

        let mut bad = cfg();
        bad.shell_content_result = ["sh".to_string()].into_iter().collect();
        CodemodePlugin::validate(&bad)
            .expect_err("a content-result name that is not a shell tool never applies");
    }
}
