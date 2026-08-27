//! Invariant: this crate suppresses WAKES, never DELIVERIES. §5 says ordinary mail QUEUES for a
//! dormant agent, so `mail/delivered` still lands, the message still splices, and the consumed set
//! simply does not grow — the standing invariant then drains the backlog in ONE wake at
//! reactivation. Suppressing delivery instead would lose the backlog, and dormancy would stop
//! being reversible.
//!
//! The suppression point is `agent/wake-request` (P5-D1), dispatched by every loop Provider
//! immediately before it opens a wake: `agent/pre-step` is too late, because it rejects a claim
//! inside an already-durable wake.

pub mod admit;
pub mod command;
pub mod error;
pub mod fold;
pub mod invariant;
pub mod vocabulary;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError, ServiceKey};
use bough_plugin_agents::{Admit, AgentWakeRequest, Agents, WakeCause, WakeKind, WakeRequest};
use bough_plugin_ledger::{
    AgentName, Append, Cite, Class, Ledger, Order, Ref, StepId, StepQuery, StepType, TrajId, WakeId,
};
use bough_plugin_rollups::Attribution;
use chrono::{DateTime, Utc};

pub use admit::{admits, Decision, ReactivateCause};
pub use error::DormancyError;
pub use fold::dormant_from;
pub use vocabulary::{AgentDormancy, OWNER, STEP_TYPE};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "dormancy";

/// The `dormancy` service key.
pub struct Dormancy;

impl ServiceKey for Dormancy {
    type Value = DormancyHandle;
    const NAME: &'static str = "dormancy";
}

/// The concrete handle newtype the key's value is (Decision D5).
#[derive(Clone)]
pub struct DormancyHandle(pub Arc<DormancyInner>);

/// One lane's cached fold: what it folded to, and the step that says so.
#[derive(Clone, Debug, Default)]
struct Cached {
    dormant: bool,
    step: Option<StepId>,
}

/// The seam's live state: the cache of the fold, and the seams it folds over.
pub struct DormancyInner {
    ledger: bough_plugin_ledger::LedgerHandle,
    agents: bough_plugin_agents::AgentsHandle,
    /// The CACHE. Never a database read on the admission path — admission runs before every wake.
    cache: parking_lot::Mutex<BTreeMap<String, Cached>>,
}

/// A request to put a lane to sleep.
#[derive(Clone, Debug)]
pub struct SleepRequest {
    pub agent: AgentName,
    pub reason: String,
    pub by: Attribution,
    pub cites: Vec<Cite>,
    pub at: DateTime<Utc>,
}

/// A request to wake a lane up.
#[derive(Clone, Debug)]
pub struct WakeUpRequest {
    pub agent: AgentName,
    pub cause: ReactivateCause,
    pub cites: Vec<Cite>,
    pub at: DateTime<Utc>,
}

/// What one sleep or reactivation did.
#[derive(Clone, Debug, PartialEq)]
pub struct DormancyChange {
    pub agent: AgentName,
    pub dormant: bool,
    pub step: StepId,
    /// `Some` when reactivation armed the drain the standing invariant demands.
    pub drain: Option<WakeId>,
}

impl DormancyHandle {
    /// An empty cache over the two bound seams.
    pub fn new(
        ledger: bough_plugin_ledger::LedgerHandle,
        agents: bough_plugin_agents::AgentsHandle,
    ) -> DormancyHandle {
        DormancyHandle(Arc::new(DormancyInner {
            ledger,
            agents,
            cache: parking_lot::Mutex::new(BTreeMap::new()),
        }))
    }

    /// The cache of the fold. Never a database read on the admission path.
    pub fn is_dormant(&self, agent: &AgentName) -> bool {
        self.0
            .cache
            .lock()
            .get(agent.as_str())
            .map(|c| c.dormant)
            .unwrap_or(false)
    }

    /// Every dormant lane, name-ordered (the map is a `BTreeMap`, so it already is).
    pub fn dormant(&self) -> Vec<AgentName> {
        self.0
            .cache
            .lock()
            .iter()
            .filter(|(_, c)| c.dormant)
            .map(|(n, _)| AgentName::new(n))
            .collect()
    }

    /// Put a lane to sleep. Appends `agent/dormancy { dormant: true }`, citing what justified it.
    /// Sleeping an already-dormant lane is IDEMPOTENT: no second step, the first one's id back.
    pub async fn sleep(&self, req: SleepRequest) -> Result<DormancyChange, DormancyError> {
        let traj = self.traj(&req.agent).await?;
        self.reload(&req.agent).await?;
        if let Some(step) = self.already(&req.agent, true) {
            return Ok(DormancyChange {
                agent: req.agent,
                dormant: true,
                step,
                drain: None,
            });
        }
        let step = self
            .append(
                &traj,
                AgentDormancy {
                    dormant: true,
                    reason: req.reason,
                    by: req.by,
                    cause: None,
                },
                req.cites,
                req.at,
            )
            .await?;
        self.remember(&req.agent, true, step.clone());
        Ok(DormancyChange {
            agent: req.agent,
            dormant: true,
            step,
            drain: None,
        })
    }

    /// Wake a lane up. Appends `agent/dormancy { dormant: false }` and, if unconsumed ordinary
    /// mail exists, requests ONE drain wake — §5's standing invariant is what drains the backlog.
    pub async fn wake_up(&self, req: WakeUpRequest) -> Result<DormancyChange, DormancyError> {
        let change = self
            .reactivate(&req.agent, req.cause, req.cites, req.at)
            .await?;
        // The drain is armed AFTER the step, so the admission listener the request passes through
        // already sees a live lane. `request_wake` answers `Nothing` when there is nothing queued,
        // which is exactly §5's "and none when nothing is queued".
        let drain = match self.0.agents.by_name(&req.agent) {
            Some(agent) => match agent
                .request_wake(WakeKind::Drain, WakeCause::Reactivated)
                .await
            {
                WakeRequest::Started(wake) => Some(wake),
                WakeRequest::Nothing => None,
            },
            None => None,
        };
        Ok(DormancyChange { drain, ..change })
    }

    /// Rebuild one agent's state from the ledger fold. Called at activation for every row.
    pub async fn reload(&self, agent: &AgentName) -> Result<bool, DormancyError> {
        let traj = self.traj(agent).await?;
        let steps = self
            .0
            .ledger
            .0
            .steps(&StepQuery {
                trajs: vec![traj],
                kinds: vec![StepType::new(STEP_TYPE)],
                order: Order::SeqDesc,
                limit: Some(1),
                ..Default::default()
            })
            .await
            .map_err(|e| DormancyError::FoldUnreadable {
                agent: agent.clone(),
                detail: e.to_string(),
            })?;
        let dormant = fold::dormant_from(&steps);
        let step = steps.first().map(|s| s.id.clone());
        self.0.cache.lock().insert(
            agent.to_string(),
            Cached {
                dormant,
                step: step.clone(),
            },
        );
        Ok(dormant)
    }

    /// Reload every `agents` row. Called at activation, so a restart does not wake a sleeping
    /// lane and does not put a live one to bed.
    pub async fn reload_all(&self) -> Result<usize, DormancyError> {
        let rows = self.0.ledger.0.agents().await?;
        let mut n = 0;
        for row in rows {
            self.reload(&row.name).await?;
            n += 1;
        }
        Ok(n)
    }

    /// The wake classes a row asked for. Read from the row, and ONLY on the dormant path: a live
    /// agent's admission never touches the store.
    pub async fn wake_classes(&self, agent: &AgentName) -> BTreeSet<String> {
        match self.0.ledger.0.agent(agent).await {
            Ok(Some(row)) => row.wake_classes,
            _ => BTreeSet::new(),
        }
    }

    /// The reactivation half, without the drain: what the admission listener runs INSIDE a wake
    /// request. Arming a drain from there would ask for a second wake while the first is still
    /// being admitted; the wake being admitted is the drain.
    pub async fn reactivate(
        &self,
        agent: &AgentName,
        cause: ReactivateCause,
        cites: Vec<Cite>,
        at: DateTime<Utc>,
    ) -> Result<DormancyChange, DormancyError> {
        let traj = self.traj(agent).await?;
        if self.0.cache.lock().get(agent.as_str()).is_none() {
            self.reload(agent).await?;
        }
        if let Some(step) = self.already(agent, false) {
            return Ok(DormancyChange {
                agent: agent.clone(),
                dormant: false,
                step,
                drain: None,
            });
        }
        let step = self
            .append(
                &traj,
                AgentDormancy {
                    dormant: false,
                    reason: format!("reactivated by {cause:?}"),
                    by: Attribution::System,
                    cause: Some(cause),
                },
                cites,
                at,
            )
            .await?;
        self.remember(agent, false, step.clone());
        Ok(DormancyChange {
            agent: agent.clone(),
            dormant: false,
            step,
            drain: None,
        })
    }

    /// The step id of the lane's current state when it ALREADY folds to `want`.
    fn already(&self, agent: &AgentName, want: bool) -> Option<StepId> {
        let cache = self.0.cache.lock();
        let cached = cache.get(agent.as_str())?;
        if cached.dormant == want {
            cached.step.clone()
        } else {
            None
        }
    }

    fn remember(&self, agent: &AgentName, dormant: bool, step: StepId) {
        self.0.cache.lock().insert(
            agent.to_string(),
            Cached {
                dormant,
                step: Some(step),
            },
        );
    }

    async fn traj(&self, agent: &AgentName) -> Result<TrajId, DormancyError> {
        self.0
            .ledger
            .0
            .agent(agent)
            .await?
            .map(|row| row.traj)
            .ok_or_else(|| DormancyError::NoSuchAgent(agent.clone()))
    }

    /// One `agent/dormancy` step on the agent's OWN trajectory. EVIDENCE when it cites something
    /// (the ledger refuses evidence without cites), THOUGHT when nothing justified it but a
    /// command.
    async fn append(
        &self,
        traj: &TrajId,
        body: AgentDormancy,
        cites: Vec<Cite>,
        at: DateTime<Utc>,
    ) -> Result<StepId, DormancyError> {
        let class = if cites.is_empty() {
            Class::Thought
        } else {
            Class::Evidence
        };
        let step = self
            .0
            .ledger
            .0
            .append(Append {
                traj: traj.clone(),
                wake: bough_plugin_agents::mail::outside_wake(),
                kind: StepType::new(STEP_TYPE),
                class,
                body: serde_json::to_value(&body).expect("AgentDormancy serializes"),
                cites,
                at,
                id: None,
            })
            .await?;
        Ok(step.id)
    }
}

/// A cite naming the message that reactivated a lane. The trigger is a live inbox message, not a
/// step, so the ref is in the `msg:` namespace rather than `step:`.
pub fn trigger_cite(message: &bough_plugin_agents::MessageId) -> Cite {
    Cite {
        r#ref: Ref::new(format!("msg:{message}")),
        url: None,
    }
}

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DormancyConfig {
    /// Fold every existing `agents` row at activation, so a restart does not wake a sleeping lane.
    pub reload_at_activation: bool,
}

/// The `dormancy` row.
pub struct DormancyPlugin;

#[async_trait::async_trait]
impl Plugin for DormancyPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = DormancyConfig;

    fn inject() -> Inject {
        Inject::required(["ledger", "agents"]).union(&Inject::optional(["commands"]))
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let fail = |e: bough_kernel::KernelError| PluginError::new(entry.clone(), e);
        let ledger =
            bough_plugin_ledger::LedgerHandle(ctx.get::<Ledger>().map_err(fail)?.0.clone());
        let agents = (*ctx.get::<Agents>().map_err(fail)?).clone();

        // Model-visible ⟺ ledgered (§0.2), as an EFFECT: unloading this row leaves the type map
        // as if it had never mounted.
        ledger
            .declare_step_types(&ctx, vocabulary::step_types())
            .await?;

        let handle = DormancyHandle::new(ledger, agents);
        ctx.provide::<Dormancy>(handle.clone())
            .await
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        if cfg.reload_at_activation {
            handle
                .reload_all()
                .await
                .map_err(|e| PluginError::new(entry.clone(), anyhow::anyhow!(e.to_string())))?;
        }

        register_admission(&ctx, &handle).await?;
        command::register(&ctx, &handle).await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::no_wake_while_dormant()]
    }
}

/// The one loop-facing registration: §1's activation rule as a listener on `agent/wake-request`.
pub async fn register_admission(
    ctx: &Context,
    handle: &DormancyHandle,
) -> Result<bough_kernel::EffectHandle, PluginError> {
    let handle = handle.clone();
    ctx.on_waterfall::<AgentWakeRequest, _, _>(move |mut v, next| {
        let handle = handle.clone();
        async move {
            // Someone earlier already refused it: a second reason is not a better one.
            if matches!(v.decision, Admit::Defer { .. }) {
                return next.run(v).await;
            }
            // THE FAST PATH: a live lane costs one cache read and nothing else.
            if !handle.is_dormant(&v.agent) {
                return next.run(v).await;
            }
            let classes = handle.wake_classes(&v.agent).await;
            match admits(true, v.kind, v.trigger.as_ref(), &classes) {
                Decision::Admit => {}
                Decision::Reactivate(cause) => {
                    let cites = v
                        .trigger
                        .as_ref()
                        .map(|t| vec![trigger_cite(&t.message)])
                        .unwrap_or_default();
                    if let Err(e) = handle.reactivate(&v.agent, cause, cites, v.at).await {
                        // A reactivation that cannot be written is not a reactivation: the lane
                        // stays asleep and says why, rather than waking with no durable record.
                        v.decision = Admit::Defer {
                            by: PLUGIN_NAME,
                            reason: format!("reactivation could not be recorded: {e}"),
                        };
                    }
                }
                Decision::Defer(by) => {
                    v.decision = Admit::Defer {
                        by,
                        reason: format!("`{}` is dormant; the item queues instead", v.agent),
                    };
                }
            }
            next.run(v).await
        }
    })
    .await
}

bough_kernel::register_plugin!(DormancyPlugin);
