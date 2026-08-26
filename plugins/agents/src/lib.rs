//! Invariant: this crate is the agents SERVICE DEFINITION (§2). It owns the `agents` key, the
//! handle, the inbox, the creation transaction, the live registry, the factory slot, the
//! initiator scope and the `agent/*` vocabulary — and NOT ONE LINE of loop code. Every wake in
//! the tree runs in `agent-loop` or in `agent-loop-scripted`, through the factory slot below.
//!
//! P2-D1: it owns live state (the registry), so it IS a catalog row and provides its own key.

pub mod agent;
pub mod create;
pub mod error;
pub mod events;
pub mod factory;
pub mod ids;
pub mod initiator;
pub mod invariant;
pub mod mail;
pub mod trace;
pub mod vocabulary;

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use bough_kernel::{
    Context, EffectHandle, InvariantSpec, Plugin, PluginError, ScopeKey, ServiceKey,
};
use bough_plugin_ledger::{Ledger, LedgerHandle, StepQuery};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

pub use agent::{Agent, AgentKind, CancelCause, Session, Status, WakeCause, WakeRequest};
pub use create::{AgentDisposer, AgentSetup, CreateAgent, CreateSpec, ResumeAgent};
pub use error::AgentError;
pub use events::{
    Admit, AgentContinuation, AgentCreated, AgentDisposed, AgentInbox, AgentPreStep, AgentPreempt,
    AgentStatusChanged, AgentStep, AgentWake, AgentWakeEnd, AgentWakeRequest, AgentWakeStopping,
    Continuation, Phase, PreStep, PreStepDecision, Preempt, StatusChange, StepEvent, TriggerFacts,
    WakeAdmission, WakeEnded, WakeEvent, WakeStopping,
};
pub use factory::{AgentCell, AgentDriver, AgentFactory, Attach, ClaimSelector};
pub use ids::{AgentId, MessageId, SessionId, WorkerId};
pub use mail::{ClaimedMessage, Delivery, Inbox, InboxReceipt, MailClass, Message, Sender, Target};

/// §2's request vocabulary, re-exported so a consumer has ONE import (P2-D3). The types live in
/// `plugins/llm` because §12 puts those waterfalls in the llm Definition; the names live here
/// because §2 puts the `agent/*` vocabulary in agents.
pub use bough_plugin_llm::{
    AgentRequest, AgentRequestError, CallConfig, Recovery, RequestCall, RequestErrorCall,
    RequestFacts, WakeKind,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "agents";

/// The `agents` service key.
pub struct Agents;

impl ServiceKey for Agents {
    type Value = AgentsHandle;
    const NAME: &'static str = "agents";
}

/// The concrete handle the key's value is (Decision D5).
#[derive(Clone)]
pub struct AgentsHandle(pub Arc<AgentsInner>);

/// The seam's live state: the registry and the factory slot.
pub struct AgentsInner {
    ctx: Context,
    ledger: LedgerHandle,
    live: Mutex<BTreeMap<String, Agent>>,
    /// Names a `create`/`resume` is holding but has not registered yet. §2 makes creation a
    /// transaction, and a transaction that publishes its name only at the END lets two concurrent
    /// creates of one name both proceed and orphan the first.
    reserved: Mutex<std::collections::BTreeSet<String>>,
    factory: Mutex<Option<Arc<dyn AgentFactory>>>,
}

impl AgentsHandle {
    /// An empty registry with no factory.
    pub fn new(ctx: Context, ledger: LedgerHandle) -> AgentsHandle {
        AgentsHandle(Arc::new(AgentsInner {
            ctx,
            ledger,
            live: Mutex::new(BTreeMap::new()),
            reserved: Mutex::new(std::collections::BTreeSet::new()),
            factory: Mutex::new(None),
        }))
    }

    /// §2: errors if one is already set, naming the driver that holds it. The token is an EFFECT,
    /// so unloading the driver row frees the slot and another loop Provider can take it — which
    /// is exactly what the phase's swap test does.
    pub async fn set_factory(
        &self,
        ctx: &Context,
        f: Arc<dyn AgentFactory>,
    ) -> Result<EffectHandle, AgentError> {
        {
            let mut slot = self.0.factory.lock();
            if let Some(held) = slot.as_ref() {
                return Err(AgentError::FactoryAlreadySet(held.driver()));
            }
            *slot = Some(f.clone());
        }
        let inner = self.0.clone();
        let mine = f;
        ctx.effect(move |e| async move {
            e.defer_sync(move || {
                let mut slot = inner.factory.lock();
                // Only free the slot if it is still OURS: a later taker is not ours to evict.
                if slot.as_ref().is_some_and(|held| Arc::ptr_eq(held, &mine)) {
                    *slot = None;
                }
            });
            Ok(())
        })
        .await
        .map_err(|e| AgentError::SetupFailed {
            name: bough_plugin_ledger::AgentName::new("<factory>"),
            detail: e.to_string(),
        })
    }

    /// The factory, if one is set.
    pub fn factory(&self) -> Option<Arc<dyn AgentFactory>> {
        self.0.factory.lock().clone()
    }

    /// The creation transaction of §2: session → agent → scope → `setup` → registry →
    /// `agent/created`, with full rollback on a `setup` failure.
    pub async fn create(&self, req: CreateAgent) -> Result<(Agent, AgentDisposer), AgentError> {
        let spec = self.resolve_create(&req);
        // The name is claimed for the whole transaction, not checked at the start and registered
        // at the end.
        let _claim = self.claim_name(&spec.name)?;
        let factory = self.factory().ok_or(AgentError::NoFactory)?;

        let (agent, scope) = self.mint(&spec, req.at);

        // `setup` runs while BOTH ids are still unpublished, so an `Err` here can be rolled back
        // with nothing to undo but the scope.
        if let Some(setup) = &req.setup {
            if let Err(e) = setup.setup(&agent).await {
                scope.dispose().await;
                return Err(AgentError::SetupFailed {
                    name: spec.name,
                    detail: e.to_string(),
                });
            }
        }

        // Only past `setup` does anything become durable: the agent row, then the seed mail.
        let seeded = match self.publish(&agent, &spec, &req).await {
            Ok(seeded) => seeded,
            Err(e) => {
                scope.dispose().await;
                return Err(e);
            }
        };

        match factory
            .attach(
                AgentCell {
                    agent: agent.clone(),
                },
                Attach::Created,
            )
            .await
        {
            Ok(driver) => *agent.0.driver.lock() = Some(driver),
            Err(e) => {
                // §2: creation is a TRANSACTION, and `attach` is inside it. The seed mail is
                // spliced back out durably — leaving it would strand unconsumed mail in an
                // inbox with no live agent and nothing that could ever drain it. The `agents`
                // row is idempotent (`put_agent` writes it only when absent, and the next
                // `create` of this name writes exactly the same row), so it is a reservation
                // rather than an orphan; that is stated here because it is the one durable
                // artefact the rollback does not remove.
                for (receipt, _) in &seeded {
                    let _ = agent
                        .inbox()
                        .discard_seed(&receipt.message, receipt.target, req.at)
                        .await;
                }
                agent.clear_pending_wake();
                scope.dispose().await;
                return Err(e);
            }
        }

        // The seed is spliced BEFORE the driver exists (it is part of the durable transaction),
        // so nothing has told the driver about it. Replaying the receipts here is what makes a
        // seeded agent actually wake: without it a worker created with its task in the seed sits
        // idle forever and `when_idle()` returns before it ever ran a step.
        if let Some(driver) = agent.driver() {
            for (receipt, msg) in &seeded {
                driver.notify(receipt, msg).await;
            }
        }

        self.0
            .live
            .lock()
            .insert(spec.name.to_string(), agent.clone());
        self.0.ctx.emit::<AgentCreated>(agent.clone());
        Ok((
            agent.clone(),
            AgentDisposer {
                agent,
                scope,
                agents: self.clone(),
            },
        ))
    }

    /// Resume an agent that already has a row and a chain: the inbox is rebuilt from
    /// `inbox/spliced` (P2-D8) and the factory attaches with [`Attach::Resumed`].
    pub async fn resume(&self, req: ResumeAgent) -> Result<(Agent, AgentDisposer), AgentError> {
        let _claim = self.claim_name(&req.name)?;
        let factory = self.factory().ok_or(AgentError::NoFactory)?;
        let row = self
            .0
            .ledger
            .0
            .agent(&req.name)
            .await?
            .ok_or_else(|| AgentError::NoSuchAgent(req.name.clone()))?;
        let spec = CreateSpec {
            name: req.name.clone(),
            traj: row.traj.clone(),
            kind: AgentKind::Resident,
            scope: ScopeKey::new(format!("agent:{}", req.name)),
        };
        let (agent, scope) = self.mint(&spec, req.at);

        // The fold IS the inbox (P2-D8): the same function crash repair uses.
        let steps = self
            .0
            .ledger
            .0
            .steps(&StepQuery {
                trajs: vec![row.traj.clone()],
                kinds: vec![bough_plugin_ledger::StepType::new("inbox/spliced")],
                ..Default::default()
            })
            .await?;
        agent.inbox().seed(Inbox::rebuild(&steps));

        if let Some(setup) = &req.setup {
            if let Err(e) = setup.setup(&agent).await {
                scope.dispose().await;
                return Err(AgentError::SetupFailed {
                    name: spec.name,
                    detail: e.to_string(),
                });
            }
        }
        match factory
            .attach(
                AgentCell {
                    agent: agent.clone(),
                },
                Attach::Resumed,
            )
            .await
        {
            Ok(driver) => *agent.0.driver.lock() = Some(driver),
            Err(e) => {
                // A resume publishes nothing durable of its own — the row and the chain were
                // already there — so the rollback is the scope and the live handle.
                scope.dispose().await;
                return Err(e);
            }
        }

        self.0
            .live
            .lock()
            .insert(spec.name.to_string(), agent.clone());
        self.0.ctx.emit::<AgentCreated>(agent.clone());
        Ok((
            agent.clone(),
            AgentDisposer {
                agent,
                scope,
                agents: self.clone(),
            },
        ))
    }

    /// The private session, the concrete agent and the scoped context — the part of the
    /// transaction that touches nothing durable and nothing published.
    fn mint(&self, spec: &CreateSpec, at: chrono::DateTime<chrono::Utc>) -> (Agent, EffectHandle) {
        let id = AgentId::new(uuid::Uuid::now_v7().to_string());
        let session = Session {
            id: SessionId::new(uuid::Uuid::now_v7().to_string()),
            traj: spec.traj.clone(),
            created_at: at,
        };
        let guard = bough_kernel::scope::create_scope(&self.0.ctx, spec.scope.clone());
        let agent = Agent(Arc::new(agent::AgentInner {
            id: id.clone(),
            name: spec.name.clone(),
            kind: spec.kind,
            session,
            inbox: Inbox::new(self.0.ledger.clone(), spec.traj.clone(), id),
            ledger: self.0.ledger.clone(),
            base: self.0.ctx.clone(),
            ctx: guard.context().clone(),
            scope_key: spec.scope.clone(),
            status: Mutex::new(Status::Idle),
            cancelled: Mutex::new(None),
            token: Mutex::new(CancellationToken::new()),
            disposed: AtomicBool::new(false),
            pending_wake: AtomicBool::new(false),
            driver: Mutex::new(None),
            idle: tokio::sync::Notify::new(),
        }));
        (agent, guard.effect().clone())
    }

    /// The durable half of the transaction: the agent row, then the seed mail.
    async fn publish(
        &self,
        agent: &Agent,
        spec: &CreateSpec,
        req: &CreateAgent,
    ) -> Result<Vec<(InboxReceipt, Message)>, AgentError> {
        if self.0.ledger.0.agent(&spec.name).await?.is_none() {
            self.0
                .ledger
                .0
                .put_agent(bough_plugin_ledger::AgentRow {
                    name: spec.name.clone(),
                    traj: spec.traj.clone(),
                    routing_refs: Default::default(),
                    wake_classes: Default::default(),
                    model_override: None,
                    tick_floor: None,
                    digest_rollup: None,
                })
                .await?;
        }
        let mut seeded = Vec::with_capacity(req.seed.len());
        for (msg, target) in &req.seed {
            // Wake-class seed mail (and anything from Andrey) is a wake: the same rule `send`
            // applies, so a seeded agent and a messaged one behave identically.
            let wake = msg.is_andrey() || msg.class == MailClass::Wake;
            let receipt = agent
                .inbox()
                .insert_waking(msg.clone(), *target, wake)
                .await?;
            if wake {
                agent.arm_pending_wake();
            }
            seeded.push((receipt, msg.clone()));
        }
        Ok(seeded)
    }

    /// Remove one agent from the live registry. The disposer is the only caller (§2: teardown is
    /// a capability).
    pub(crate) fn detach(&self, id: &AgentId) {
        self.0.live.lock().retain(|_, a| a.id() != id);
    }

    pub fn get(&self, id: &AgentId) -> Option<Agent> {
        self.0.live.lock().values().find(|a| a.id() == id).cloned()
    }
    /// Claim a name for the length of a creation transaction. Dropping the guard frees it.
    fn claim_name(&self, name: &bough_plugin_ledger::AgentName) -> Result<NameClaim, AgentError> {
        if self.0.live.lock().contains_key(name.as_str()) {
            return Err(AgentError::AlreadyLive(name.clone()));
        }
        if !self.0.reserved.lock().insert(name.to_string()) {
            return Err(AgentError::AlreadyLive(name.clone()));
        }
        Ok(NameClaim {
            agents: self.clone(),
            name: name.to_string(),
        })
    }

    pub fn by_name(&self, name: &bough_plugin_ledger::AgentName) -> Option<Agent> {
        self.0.live.lock().get(name.as_str()).cloned()
    }
    pub fn list(&self) -> Vec<Agent> {
        self.0.live.lock().values().cloned().collect()
    }
    /// The explicit defaulting step (§0.2): never a `?? default` inside `create`.
    pub fn resolve_create(&self, req: &CreateAgent) -> CreateSpec {
        CreateSpec {
            name: req.name.clone(),
            traj: req.traj.clone(),
            kind: req.kind,
            scope: req
                .scope
                .clone()
                .unwrap_or_else(|| ScopeKey::new(format!("agent:{}", req.name))),
        }
    }
}

/// No configuration: everything deployment-varying about a wake belongs to the loop row.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentsConfig {}

/// The Service Definition row.
pub struct AgentsPlugin;

#[async_trait::async_trait]
impl Plugin for AgentsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = AgentsConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["ledger"])
    }

    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let ledger = (*ledger).clone();

        ledger
            .declare_step_types(&ctx, vocabulary::step_types())
            .await?;

        ctx.provide::<Agents>(AgentsHandle::new(ctx.clone(), ledger))
            .await
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        // The recorded stream this crate's invariant reads. Per fiber LIFE, exactly as the
        // ledger's is (§0.3): a reload must not read as a violation of its own predecessor.
        let mine = ctx.fiber_uid();
        ctx.effect(move |e| async move {
            e.defer_sync(move || invariant::forget(mine));
            Ok(())
        })
        .await?;
        ctx.on::<AgentStatusChanged, _, _>(move |change| async move {
            invariant::record(invariant::Obs::Status {
                fiber: mine,
                agent: change.agent,
                from: change.from,
                to: change.to,
            });
        })
        .await?;
        ctx.on::<AgentDisposed, _, _>(move |agent| async move {
            invariant::record(invariant::Obs::Disposed { fiber: mine, agent });
        })
        .await?;
        ctx.on::<AgentWake, _, _>(move |wake| async move {
            if wake.phase == Phase::Start {
                invariant::record(invariant::Obs::WakeStarted {
                    fiber: mine,
                    agent: wake.agent,
                });
            }
        })
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::agent_lifecycle_is_sane()]
    }
}

bough_kernel::register_plugin!(AgentsPlugin);

/// The name a creation transaction holds until it registers or rolls back.
struct NameClaim {
    agents: AgentsHandle,
    name: String,
}

impl Drop for NameClaim {
    fn drop(&mut self) {
        self.agents.0.reserved.lock().remove(&self.name);
    }
}
