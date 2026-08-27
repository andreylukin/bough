//! Invariant (§17 Phase 8): this row breaks exactly one NAMED site, exactly as often as it says,
//! and counts every hit. It is CATALOG-ONLY (decision D-C8): compiled into the binary, named by no
//! bundle, mounted by a test's own `--patch`, and invisible to `--dump-config` on every shipped
//! profile.
//!
//! The counters are process-global, so a test that mounts this row holds [`test_lock`] for its
//! whole body (the `hello::trace` precedent).

pub mod invariant;
pub mod sites;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::AgentName;
use bough_plugin_projection::ProjectionError;
use bough_plugin_projection::{
    DropPriority, Position, Projection, SectionBody, SectionId, SectionRender, SectionRequest,
    SectionScope, SectionSpec, Slot,
};
use bough_plugin_tools::{
    FailureClass, RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolName, ToolOutcome,
    ToolScope, ToolSpec, Tools,
};

pub use crate::sites::{FaultKind, FaultSite};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "fault-inject";

/// The section id this row contributes for [`FaultSite::ProjectionSection`]. A PROTOCOL name: a
/// test asserts the assembler's error names this section rather than the assembler.
pub const FAULT_SECTION: &str = "fault";

/// The tool name this row registers for [`FaultSite::ToolExecute`].
pub const FAULT_TOOL: &str = "fault";

/// The row's config. One site per row, so a test names what it broke.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FaultConfig {
    /// WHERE.
    pub at: FaultSite,
    /// HOW.
    pub how: FaultKind,
    /// Fire on the Nth hit of the site, 1-based. A PROTOCOL counter: it is what makes "and the
    /// loop CONTINUES" observable — fail wake 1, pass wake 2.
    pub after: u32,
    /// Fire this many times then stop. `0` = forever.
    pub times: u32,
    /// Restrict to one agent. `None` = every agent.
    pub agent: Option<AgentName>,
}

/// Hits recorded for `site` this process.
pub fn hits(site: FaultSite) -> u32 {
    sites::hits(site)
}

/// How many times `apply` ran. The "not retried" evidence: a FAILED row's `apply` is called once
/// and never again, and this counter is what a test reads to say so.
pub fn applies() -> u32 {
    APPLIES.load(Ordering::SeqCst)
}

static APPLIES: AtomicU32 = AtomicU32::new(0);

/// Zero every counter. A test's setup.
pub fn reset() {
    APPLIES.store(0, Ordering::SeqCst);
    sites::clear();
}

/// The lock a test holds for its whole body: the counters are process-global.
///
/// DEVIATION from the plan's signature (`std::sync::MutexGuard`): the crate's mutexes are
/// `parking_lot`'s like everywhere else in the tree, so the guard is `parking_lot`'s, wrapped the
/// way `hello::trace::test_lock` wraps its own (and poison-free for the same reason).
pub fn test_lock() -> TestGuard {
    static TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    let guard = TEST_LOCK.lock();
    reset();
    TestGuard(guard)
}

/// The guard [`test_lock`] returns. Holding it IS its purpose.
#[allow(dead_code)]
pub struct TestGuard(parking_lot::MutexGuard<'static, ()>);

/// PURE: whether the next hit of this row's site fires, given the hit number.
fn fires(cfg: &FaultConfig, hit: u32) -> bool {
    sites::fires(hit, cfg.after, cfg.times)
}

/// The failure this row raises, as an `anyhow` error naming the site.
fn boom(cfg: &FaultConfig) -> anyhow::Error {
    anyhow::anyhow!("fault-inject: injected failure at `{}`", cfg.at.as_str())
}

/// Fire: an `Err` value, or a panic, per [`FaultKind`].
fn fire(cfg: &FaultConfig) -> anyhow::Error {
    match cfg.how {
        FaultKind::Error => boom(cfg),
        FaultKind::Panic => panic!("fault-inject: injected panic at `{}`", cfg.at.as_str()),
    }
}

/// The contributed section that fails on purpose.
struct FaultSection {
    cfg: Arc<FaultConfig>,
}

#[async_trait::async_trait]
impl SectionRender for FaultSection {
    async fn render(&self, req: &SectionRequest) -> Result<Option<SectionBody>, ProjectionError> {
        if !sites::applies_to(self.cfg.agent.as_ref(), &req.agent) {
            return Ok(None);
        }
        let hit = sites::hit(FaultSite::ProjectionSection);
        if !fires(&self.cfg, hit) {
            return Ok(None);
        }
        Err(ProjectionError::SectionRender {
            id: SectionId::new(FAULT_SECTION),
            detail: fire(&self.cfg).to_string(),
        })
    }
}

/// The failing section renderer, for a test that drives a real assembler directly rather than
/// mounting this row (the section fault is only observable through an assembly).
pub fn test_section(cfg: Arc<FaultConfig>) -> impl SectionRender {
    FaultSection { cfg }
}

/// The registered tool that fails on purpose.
struct FaultTool {
    cfg: Arc<FaultConfig>,
}

#[async_trait::async_trait]
impl Tool for FaultTool {
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        if !sites::applies_to(self.cfg.agent.as_ref(), &call.agent) {
            return Ok(ToolOutcome::default());
        }
        let hit = sites::hit(FaultSite::ToolExecute);
        if !fires(&self.cfg, hit) {
            return Ok(ToolOutcome::default());
        }
        Err(ToolFailure {
            kind: FailureClass::Error,
            message: fire(&self.cfg).to_string(),
        })
    }
}

/// The row.
pub struct FaultInjectPlugin;

#[async_trait::async_trait]
impl Plugin for FaultInjectPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = FaultConfig;

    fn inject() -> Inject {
        Inject::optional(["projection", "tools", "agents"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        if cfg.after == 0 {
            return Err(ConfigError::Rejected {
                detail: "after: hits are 1-based, so `0` names no hit; the first hit is `1`".into(),
            });
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        APPLIES.fetch_add(1, Ordering::SeqCst);
        let entry = ctx.entry_id().clone();
        let missing = |key: &str| {
            PluginError::new(
                entry.clone(),
                anyhow::anyhow!(
                    "fault-inject: the `{key}` seam is not mounted, so the `{}` site cannot be armed",
                    cfg.at.as_str()
                ),
            )
        };

        match cfg.at {
            // The row's own apply fails: the fiber goes FAILED. The one deliberate FAILED row.
            FaultSite::Apply => {
                let hit = sites::hit(FaultSite::Apply);
                if fires(&cfg, hit) {
                    return Err(PluginError::new(entry, fire(&cfg)));
                }
                Ok(())
            }
            FaultSite::ProjectionSection => {
                let projection = ctx
                    .try_get::<Projection>()
                    .map_err(|e| PluginError::new(entry.clone(), e))?
                    .ok_or_else(|| missing("projection"))?;
                projection
                    .section(
                        &ctx,
                        SectionSpec {
                            id: SectionId::new(FAULT_SECTION),
                            position: Position {
                                slot: Slot::Tail,
                                place: bough_plugin_projection::Place::After,
                            },
                            scope: SectionScope::Global,
                            agent: None,
                            priority: DropPriority::Never,
                            render: Arc::new(FaultSection { cfg: cfg.clone() }),
                        },
                    )
                    .await?;
                Ok(())
            }
            FaultSite::ToolExecute => {
                let tools = ctx
                    .try_get::<Tools>()
                    .map_err(|e| PluginError::new(entry.clone(), e))?
                    .ok_or_else(|| missing("tools"))?;
                tools
                    .register(
                        &ctx,
                        ToolSpec {
                            name: ToolName::new(FAULT_TOOL),
                            description: "fails on purpose (fault-inject)".into(),
                            input_schema: schemars::Schema::try_from(serde_json::json!({
                                "type": "object",
                                "properties": {},
                            }))
                            .expect("the fault tool's schema is an object"),
                            render: RenderIntent::Generic,
                            scope: ToolScope::Global,
                            tool: Arc::new(FaultTool { cfg: cfg.clone() }),
                        },
                    )
                    .await?;
                Ok(())
            }
            // `agent/wake-stopping` is SERIAL with an UNINHABITED output (P2-D10): a listener has
            // no failure channel at all, so `error` is recorded and reported, and `panic` is the
            // only way to make the listener itself fail. Recorded in docs/track-c-merge-notes.md.
            FaultSite::WakeStopping => {
                let cfg = cfg.clone();
                ctx.on_serial::<bough_plugin_agents::AgentWakeStopping, _, _>(move |p| {
                    let cfg = cfg.clone();
                    async move {
                        let agent = AgentName::new(p.agent.as_str());
                        if sites::applies_to(cfg.agent.as_ref(), &agent) {
                            let hit = sites::hit(FaultSite::WakeStopping);
                            if fires(&cfg, hit) {
                                let err = fire(&cfg);
                                tracing::error!(error = %err, "fault-inject: wake-stopping fault");
                            }
                        }
                        None
                    }
                })
                .await?;
                Ok(())
            }
        }
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(FaultInjectPlugin);
