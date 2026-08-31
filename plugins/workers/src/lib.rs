//! Invariant: this crate is the workers SERVICE DEFINITION (§10). It owns the `workers` key, the
//! start/result vocabulary, the live-run table, the provider registry and the THREE BOUNDS — and
//! no spawning. The bounds are checked HERE so every provider obeys the same numbers (§7), and a
//! provider that wanted its own would have to lie about the seam.
//!
//! P2-D1: it owns live state (the run table), so it IS a catalog row and provides its own key.

pub mod error;
pub mod ids;
pub mod invariant;
pub mod run;
pub mod seal;
pub mod start;
pub mod vocabulary;

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, InvariantSpec, Plugin, PluginError, ServiceKey};
use bough_plugin_ledger::{Append, Class, Ledger, LedgerHandle, StepType, TrajId, WakeId};
use parking_lot::Mutex;

pub use error::WorkerError;
pub use ids::WorkerId;
pub use run::{AskAnswer, AskSink, AskedQuestion, NullAskSink, WorkerRun};
pub use seal::{worker_ref, Report, ReportClaim, SealSpec};

/// The union of a report's external cites: what a spawner may carry forward as evidence (§10).
pub fn external_cites_of(worker: &WorkerId, report: &Report) -> Vec<bough_plugin_ledger::Cite> {
    let mut out: Vec<bough_plugin_ledger::Cite> = Vec::new();
    for claim in &report.claims {
        for cite in claim.external_cites(worker) {
            if !out.contains(&cite) {
                out.push(cite);
            }
        }
    }
    out
}

/// `tools::Restrict`, re-exported: a spawn request names one and a consumer should not need two
/// imports to build it.
pub use bough_plugin_tools::Restrict;
pub use start::{AskMode, Bounds, StartWorker, WorkerKind, WorkerOutcome, WorkerResult};
pub use vocabulary::{WorkerClaim, WorkerReport, WorkerStarted};

use bough_plugin_ledger::AgentName;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "workers";

/// How many wakes' spawn counters are kept before the oldest is forgotten. A protocol constant,
/// not a tunable: it bounds a bookkeeping map, and no deployment wants a different number.
const WAKE_COUNTER_WINDOW: usize = 256;

/// The `workers` service key.
pub struct Workers;

impl ServiceKey for Workers {
    type Value = WorkersHandle;
    const NAME: &'static str = "workers";
}

/// The concrete handle the key's value is (Decision D5).
#[derive(Clone)]
pub struct WorkersHandle(pub Arc<WorkersInner>);

/// One entry of the live-run table.
#[derive(Clone)]
struct LiveRun {
    run: WorkerRun,
    agent: AgentName,
    depth: u8,
}

/// The seam's live state: the run table, the provider registry and the bounds.
pub struct WorkersInner {
    runs: Mutex<BTreeMap<WorkerId, LiveRun>>,
    providers: Mutex<Vec<Arc<dyn WorkerProvider>>>,
    /// Per-wake spawn counters, with a bounded window so a long-lived process cannot grow one
    /// entry per wake forever. A wake not in the map has spawned nothing, which is why the cap
    /// "resets at the next wake" for free.
    per_wake: Mutex<(BTreeMap<WakeId, usize>, VecDeque<WakeId>)>,
    bounds: Bounds,
    /// The sink a run's `ask()` delivers through. `worker-spawn` installs the spawner's own lane;
    /// the default drops questions, so an unwired seam ENDS an asking worker rather than hanging.
    sink: Mutex<Arc<dyn AskSink>>,
    /// What `ask()` does when the caller does not say. The `workers` row has no opinion; the
    /// mounted provider sets it from its own config, and `tool-ask` reads it here rather than
    /// growing a config of its own for a value that must match the provider's.
    default_ask_mode: Mutex<AskMode>,
}

/// What a worker Provider does.
#[async_trait::async_trait]
pub trait WorkerProvider: Send + Sync + 'static {
    fn kinds(&self) -> Vec<WorkerKind>;
    async fn start(
        &self,
        req: Arc<StartWorker>,
        run: WorkerRun,
    ) -> Result<WorkerResult, WorkerError>;
}

impl WorkersHandle {
    /// An empty seam with the row's bounds.
    pub fn new(bounds: Bounds) -> WorkersHandle {
        WorkersHandle(Arc::new(WorkersInner {
            runs: Mutex::new(BTreeMap::new()),
            providers: Mutex::new(Vec::new()),
            per_wake: Mutex::new((BTreeMap::new(), VecDeque::new())),
            bounds,
            sink: Mutex::new(Arc::new(NullAskSink)),
            default_ask_mode: Mutex::new(AskMode::Block),
        }))
    }

    /// Register a Provider. Registration is an effect (§0.2).
    pub async fn provider(
        &self,
        ctx: &Context,
        p: Arc<dyn WorkerProvider>,
    ) -> Result<EffectHandle, PluginError> {
        let inner = self.0.clone();
        ctx.effect(move |ectx| async move {
            let key = Arc::as_ptr(&p) as *const () as usize;
            inner.providers.lock().push(p);
            ectx.defer_sync(move || {
                inner
                    .providers
                    .lock()
                    .retain(|q| Arc::as_ptr(q) as *const () as usize != key);
            });
            Ok(())
        })
        .await
    }

    /// Install the lane a run's `ask()` delivers on. An effect, like every registration: the
    /// inverse puts the null sink back, so unloading `worker-spawn` leaves no trace.
    pub async fn ask_sink(
        &self,
        ctx: &Context,
        sink: Arc<dyn AskSink>,
    ) -> Result<EffectHandle, PluginError> {
        let inner = self.0.clone();
        ctx.effect(move |ectx| async move {
            *inner.sink.lock() = sink;
            ectx.defer_sync(move || *inner.sink.lock() = Arc::new(NullAskSink));
            Ok(())
        })
        .await
    }

    /// The mounted provider's `ask` mode. An effect, so unloading it restores the default.
    pub async fn set_default_ask_mode(
        &self,
        ctx: &Context,
        mode: AskMode,
    ) -> Result<EffectHandle, PluginError> {
        let inner = self.0.clone();
        ctx.effect(move |ectx| async move {
            let was = std::mem::replace(&mut *inner.default_ask_mode.lock(), mode);
            ectx.defer_sync(move || *inner.default_ask_mode.lock() = was);
            Ok(())
        })
        .await
    }

    /// What `ask()` does when the caller does not say.
    pub fn default_ask_mode(&self) -> AskMode {
        *self.0.default_ask_mode.lock()
    }

    /// Bounds are checked HERE, in the Definition, so every provider obeys the same numbers (§7).
    /// A kind no Provider registered is [`WorkerError::NoProvider`].
    ///
    /// Order is deliberate: depth first (it is a property of the request alone), then the global
    /// in-flight count, then the per-wake cap. A refusal NAMES its bound.
    pub async fn start(
        &self,
        ctx: &Context,
        req: StartWorker,
    ) -> Result<WorkerResult, WorkerError> {
        let b = &self.0.bounds;
        if req.depth as usize > b.max_depth as usize {
            return Err(WorkerError::BoundsExceeded {
                bound: "max_depth",
                current: req.depth as usize,
                limit: b.max_depth as usize,
            });
        }
        let provider = self
            .provider_for(req.kind)
            .ok_or(WorkerError::NoProvider(req.kind))?;

        // Reserve in ONE critical section: two concurrent starts must not both see room.
        let id = WorkerId::new(uuid::Uuid::now_v7().to_string());
        {
            let mut runs = self.0.runs.lock();
            if runs.len() >= b.max_in_flight {
                return Err(WorkerError::BoundsExceeded {
                    bound: "max_in_flight",
                    current: runs.len(),
                    limit: b.max_in_flight,
                });
            }
            let mut per_wake = self.0.per_wake.lock();
            let spawned = *per_wake.0.get(&req.wake).unwrap_or(&0);
            if spawned >= b.per_wake_spawn_cap {
                return Err(WorkerError::BoundsExceeded {
                    bound: "per_wake_spawn_cap",
                    current: spawned,
                    limit: b.per_wake_spawn_cap,
                });
            }
            let (counts, order) = &mut *per_wake;
            if counts.insert(req.wake.clone(), spawned + 1).is_none() {
                order.push_back(req.wake.clone());
                while order.len() > WAKE_COUNTER_WINDOW {
                    if let Some(old) = order.pop_front() {
                        counts.remove(&old);
                    }
                }
            }
            let sink = self.0.sink.lock().clone();
            runs.insert(
                id.clone(),
                LiveRun {
                    run: WorkerRun::new(id.clone(), req.spawner.clone(), req.ask_mode, sink),
                    agent: worker_agent_name(&req.spawner, &id),
                    depth: req.depth,
                },
            );
            invariant::record(invariant::Obs::Started {
                fiber: ctx.fiber_uid(),
                worker: id.clone(),
                depth: req.depth,
                in_flight_after: runs.len(),
            });
        }

        let run = self
            .0
            .runs
            .lock()
            .get(&id)
            .map(|l| l.run.clone())
            .expect("just inserted");
        let req = Arc::new(req);
        self.append_started(ctx, &req, &id).await;
        let out = provider.start(req, run).await;

        self.0.runs.lock().remove(&id);
        invariant::record(invariant::Obs::Finished {
            fiber: ctx.fiber_uid(),
            worker: id.clone(),
        });
        out
    }

    /// `worker/started` in the SPAWNER's chain, best-effort: without a ledger binding or an agent
    /// row there is no trajectory to write to, and the bound check above is still the authority.
    async fn append_started(&self, ctx: &Context, req: &StartWorker, id: &WorkerId) {
        let Some(traj) = spawner_traj(ctx, &req.spawner).await else {
            return;
        };
        let Ok(Some(ledger)) = ctx.try_get::<Ledger>() else {
            return;
        };
        let body = serde_json::to_value(WorkerStarted {
            worker: id.clone(),
            kind: req.kind,
            task: req.task.clone(),
            depth: req.depth,
            seal: req.seal.name.clone(),
        })
        .expect("WorkerStarted serialises");
        let _ = ledger
            .0
            .append(Append {
                traj,
                wake: req.wake.clone(),
                kind: StepType::new("worker/started"),
                class: Class::Thought,
                body,
                cites: Vec::new(),
                at: req.at,
                id: None,
            })
            .await;
    }

    fn provider_for(&self, kind: WorkerKind) -> Option<Arc<dyn WorkerProvider>> {
        self.0
            .providers
            .lock()
            .iter()
            .rev()
            .find(|p| p.kinds().contains(&kind))
            .cloned()
    }

    /// The live runs.
    pub fn live(&self) -> Vec<WorkerRun> {
        self.0.runs.lock().values().map(|l| l.run.clone()).collect()
    }

    /// The configured bounds.
    pub fn bounds(&self) -> Bounds {
        self.0.bounds.clone()
    }

    /// How many runs are in flight right now.
    pub fn in_flight(&self) -> usize {
        self.0.runs.lock().len()
    }

    /// How many workers `wake` has started so far.
    pub fn spawned_in_wake(&self, wake: &WakeId) -> usize {
        *self.0.per_wake.lock().0.get(wake).unwrap_or(&0)
    }

    /// The live run whose WORKER AGENT is `agent`, if any. `tool-ask` resolves its own run
    /// through this: a worker knows its agent name, never its worker id.
    pub fn run_for_agent(&self, agent: &AgentName) -> Option<WorkerRun> {
        self.0
            .runs
            .lock()
            .values()
            .find(|l| &l.agent == agent)
            .map(|l| l.run.clone())
    }

    /// How deep `agent` sits in the worker chain. `0` for anything that is not a live worker, so
    /// a resident's first worker is depth 1.
    pub fn depth_of(&self, agent: &AgentName) -> u8 {
        self.0
            .runs
            .lock()
            .values()
            .find(|l| &l.agent == agent)
            .map(|l| l.depth)
            .unwrap_or(0)
    }

    /// The agent name a run's worker gets. Deterministic, so `depth_of` and `run_for_agent` can
    /// answer from the run table without the provider telling them anything.
    pub fn worker_agent_name(spawner: &AgentName, id: &WorkerId) -> AgentName {
        worker_agent_name(spawner, id)
    }
}

fn worker_agent_name(spawner: &AgentName, id: &WorkerId) -> AgentName {
    AgentName::new(format!("{spawner}/worker-{id}"))
}

/// The spawner's trajectory, read from its ledger row. `None` when there is no ledger or no row.
async fn spawner_traj(ctx: &Context, spawner: &AgentName) -> Option<TrajId> {
    let ledger: Arc<LedgerHandle> = ctx.try_get::<Ledger>().ok().flatten()?;
    ledger.0.agent(spawner).await.ok().flatten().map(|r| r.traj)
}

/// The row's config: the three bounds of §7.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkersConfig {
    /// Default 8.
    pub max_in_flight: usize,
    /// Default 3.
    pub max_depth: u8,
    pub per_wake_spawn_cap: usize,
}

impl From<&WorkersConfig> for Bounds {
    fn from(c: &WorkersConfig) -> Bounds {
        Bounds {
            max_in_flight: c.max_in_flight,
            max_depth: c.max_depth,
            per_wake_spawn_cap: c.per_wake_spawn_cap,
        }
    }
}

/// The Service Definition row.
pub struct WorkersPlugin;

#[async_trait::async_trait]
impl Plugin for WorkersPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = WorkersConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["ledger"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        if cfg.max_in_flight == 0 || cfg.max_depth == 0 || cfg.per_wake_spawn_cap == 0 {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: "every worker bound must be at least 1; unmount the row to disable workers"
                    .to_string(),
            });
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        ledger
            .declare_step_types(&ctx, vocabulary::step_types())
            .await?;
        let bounds = Bounds::from(cfg.as_ref());
        invariant::set_bounds(bounds.clone());
        ctx.provide::<Workers>(WorkersHandle::new(bounds))
            .await
            .map_err(|e| PluginError::new(entry, e))?;

        // Per fiber LIFE, like the ledger's: a reload keeps the `FiberUid`, so this fiber's
        // observations are forgotten when it unloads or the invariant would flag the reload.
        let mine = ctx.fiber_uid();
        ctx.effect(move |e| async move {
            e.defer_sync(move || invariant::forget(mine));
            Ok(())
        })
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::runs_stay_within_bounds()]
    }
}

bough_kernel::register_plugin!(WorkersPlugin);
