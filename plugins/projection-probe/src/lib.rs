//! Invariant: `projection-probe` is a TEST INSTRUMENT, not a product row (P1-D16). It exists to
//! exercise the REAL catalog path for §17 Phase 1: it injects `ledger` and `projection`, declares
//! two step types (`probe/note`, and `probe/scratch` with `ignorable: true`), registers one global
//! and one agent-scoped section with the SAME `SectionId` (the shadowing fixture), appends a small
//! scripted trajectory on `apply`, and pushes every interesting moment onto a shared trace the
//! tests assert on in order. It is in no bundle; the tests' own `$BOUGH_HOME` mounts it.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::vocabulary::{
    StepEnd, StepOutcome, StepStart, Urgency, WakeEnd, WakeEndReason, WakeStart,
};
use bough_plugin_ledger::{
    AgentName, AgentRow, Append, Class, ClassRule, Ledger, LedgerHandle, Ref, StepId, StepType,
    StepTypeDef, TrajId, WakeId,
};
use bough_plugin_projection::section::{
    DropPriority, Place, Position, SectionBody, SectionCites, SectionId, SectionRender,
    SectionRequest, SectionScope, SectionSpec, Slot,
};
use bough_plugin_projection::{Projection, ProjectionError};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "projection-probe";

/// The `SectionId` the global and the agent-scoped section SHARE — the shadowing fixture.
pub const SECTION_ID: &str = "probe.note";

/// The probe's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProbeConfig {
    /// The trajectory the scripted steps are appended to.
    pub traj: String,
    /// The agent the probe's agent-scoped section shadows a global one for.
    pub agent: String,
    /// How many scripted steps `apply` appends.
    #[serde(default = "default_steps")]
    pub steps: usize,
    /// Test hook: the agent-scoped section cites a step id that is not in the ledger, so
    /// `model_visible_is_ledgered` has something real to report. V-`ledger_invariants`'s vehicle.
    #[serde(default)]
    pub plant_missing_cite: bool,
}

fn default_steps() -> usize {
    3
}

/// The body of `probe/note` — a marked thought with a line of text.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ProbeNote {
    pub text: String,
    pub index: u32,
}

/// The body of `probe/scratch` — the `ignorable: true` type (P1-D7).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ProbeScratch {
    pub scratch: String,
}

/// One recorded moment, in the `hello` trace tradition.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceLine {
    pub plugin: &'static str,
    pub moment: String,
}

static TRACE: parking_lot::Mutex<Vec<TraceLine>> = parking_lot::Mutex::new(Vec::new());
static APPENDED: parking_lot::Mutex<Vec<StepId>> = parking_lot::Mutex::new(Vec::new());

/// Everything the probe has done this process, in order.
pub fn trace() -> Vec<TraceLine> {
    TRACE.lock().clone()
}

/// Push one moment onto the shared trace.
pub fn push(plugin: &'static str, moment: impl Into<String>) {
    TRACE.lock().push(TraceLine {
        plugin,
        moment: moment.into(),
    });
}

/// Whether the trace holds this exact moment.
pub fn saw(moment: &str) -> bool {
    TRACE.lock().iter().any(|l| l.moment == moment)
}

/// Drop the trace. Test setup only.
pub fn clear() {
    TRACE.lock().clear();
    APPENDED.lock().clear();
}

/// The step ids the last `apply` appended, in seq order. The sections cite them.
pub fn appended() -> Vec<StepId> {
    APPENDED.lock().clone()
}

/// What a probe section renders. Deterministic: a fixed title and body plus the cites of the
/// steps this process appended, so the section is model-visible AND ledgered.
struct ProbeSection {
    label: &'static str,
    /// `Some` ⇒ cite this id instead of the real ones (the planted violation).
    planted: Option<StepId>,
}

#[async_trait::async_trait]
impl SectionRender for ProbeSection {
    async fn render(&self, req: &SectionRequest) -> Result<Option<SectionBody>, ProjectionError> {
        let steps = match &self.planted {
            Some(id) => vec![id.clone()],
            None => appended(),
        };
        push(PLUGIN_NAME, format!("render:{}", self.label));
        Ok(Some(SectionBody {
            title: format!("probe ({})", self.label),
            body: format!("probe section, scope={}, agent={}", self.label, req.agent),
            cites: SectionCites {
                steps,
                rollups: Vec::new(),
            },
        }))
    }
}

/// The fixture plugin.
pub struct ProbePlugin;

#[async_trait::async_trait]
impl Plugin for ProbePlugin {
    const NAME: &'static str = "projection-probe";
    type Config = ProbeConfig;

    fn inject() -> Inject {
        Inject::required(["ledger", "projection"])
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        push(Self::NAME, "apply");

        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let projection = ctx
            .get::<Projection>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        // Which provider this activation bound against. The swap test reads these two lines.
        push(Self::NAME, format!("ledger={}", ledger.0.provider()));
        push(
            Self::NAME,
            format!("projection={}", projection.0.provider()),
        );

        // Registered FIRST so it unwinds LAST: `unload` closes this fiber's teardown.
        ctx.effect(|e| async move {
            e.defer_sync(|| push(ProbePlugin::NAME, "unload"));
            Ok(())
        })
        .await?;

        // The two declared types. `probe/scratch` is the `ignorable: true` half.
        ledger
            .declare_step_types(
                &ctx,
                vec![
                    StepTypeDef::of::<ProbeNote>("probe/note", Self::NAME)
                        .class_rule(ClassRule::Either),
                    StepTypeDef::of::<ProbeScratch>("probe/scratch", Self::NAME)
                        .ignorable(true)
                        .class_rule(ClassRule::Thought),
                ],
            )
            .await?;
        push(Self::NAME, "step-types");

        let agent = AgentName::new(&cfg.agent);
        let traj = TrajId::new(&cfg.traj);
        script(&ledger, &cfg, &traj, &agent)
            .await
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        push(Self::NAME, "scripted");

        // The shadowing fixture: the SAME SectionId at global and at agent scope.
        let id = SectionId::new(SECTION_ID);
        projection
            .section(
                &ctx,
                SectionSpec {
                    id: id.clone(),
                    position: Position {
                        slot: Slot::Tail,
                        place: Place::After,
                    },
                    scope: SectionScope::Global,
                    agent: None,
                    priority: DropPriority::Never,
                    render: Arc::new(ProbeSection {
                        label: "global",
                        planted: None,
                    }),
                },
            )
            .await?;
        projection
            .section(
                &ctx,
                SectionSpec {
                    id,
                    position: Position {
                        slot: Slot::Tail,
                        place: Place::After,
                    },
                    scope: SectionScope::Agent,
                    agent: Some(agent.clone()),
                    priority: DropPriority::Never,
                    render: Arc::new(ProbeSection {
                        label: "agent",
                        planted: cfg
                            .plant_missing_cite
                            .then(|| StepId::new("step-that-was-never-appended")),
                    }),
                },
            )
            .await?;
        push(Self::NAME, "sections");

        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

/// The scripted trajectory: one closed wake enclosing `steps` closed step pairs, each carrying a
/// `probe/note`, plus one `probe/scratch`. Written through the REAL append path, so a bench or a
/// swap test is measuring the provider and not a fixture shortcut.
async fn script(
    ledger: &LedgerHandle,
    cfg: &ProbeConfig,
    traj: &TrajId,
    agent: &AgentName,
) -> Result<(), anyhow::Error> {
    let at = chrono::Utc::now();
    let wake = WakeId::new(format!("{}-w1", cfg.traj));

    ledger
        .0
        .put_agent(AgentRow {
            name: agent.clone(),
            traj: traj.clone(),
            routing_refs: [Ref::new(format!("probe:{}", cfg.agent))]
                .into_iter()
                .collect(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await?;

    let mut reqs = vec![append(
        traj,
        &wake,
        "wake/start",
        Class::Thought,
        serde_json::to_value(WakeStart {
            urgency: Urgency::Immediate,
            trigger: None,
            claimed: Vec::new(),
        })?,
        at,
    )];
    for i in 0..cfg.steps {
        let index = i as u32;
        reqs.push(append(
            traj,
            &wake,
            "step/start",
            Class::Thought,
            serde_json::to_value(StepStart { index })?,
            at,
        ));
        reqs.push(append(
            traj,
            &wake,
            "probe/note",
            Class::Thought,
            serde_json::to_value(ProbeNote {
                text: format!("probe note {index}"),
                index,
            })?,
            at,
        ));
        reqs.push(append(
            traj,
            &wake,
            "step/end",
            Class::Thought,
            serde_json::to_value(StepEnd {
                index,
                outcome: StepOutcome::Ok,
                detail: None,
            })?,
            at,
        ));
    }
    reqs.push(append(
        traj,
        &wake,
        "probe/scratch",
        Class::Thought,
        serde_json::to_value(ProbeScratch {
            scratch: "ignorable".into(),
        })?,
        at,
    ));
    reqs.push(append(
        traj,
        &wake,
        "wake/end",
        Class::Thought,
        serde_json::to_value(WakeEnd {
            reason: WakeEndReason::Completed,
            cause: None,
            consumed: Vec::new(),
        })?,
        at,
    ));

    let steps = ledger.0.append_batch(reqs).await?;
    let mut ids = APPENDED.lock();
    ids.clear();
    ids.extend(steps.into_iter().map(|s| s.id));
    Ok(())
}

fn append(
    traj: &TrajId,
    wake: &WakeId,
    kind: &str,
    class: Class,
    body: serde_json::Value,
    at: chrono::DateTime<chrono::Utc>,
) -> Append {
    Append {
        traj: traj.clone(),
        wake: wake.clone(),
        kind: StepType::new(kind),
        class,
        body,
        cites: Vec::new(),
        at,
        id: None,
    }
}

bough_kernel::register_plugin!(ProbePlugin);
