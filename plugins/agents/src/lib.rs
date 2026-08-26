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
pub mod vocabulary;

use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, InvariantSpec, Plugin, PluginError, ServiceKey};

pub use agent::{Agent, AgentKind, CancelCause, Session, Status};
pub use create::{AgentDisposer, AgentSetup, CreateAgent, CreateSpec, ResumeAgent};
pub use error::AgentError;
pub use events::{
    AgentContinuation, AgentCreated, AgentDisposed, AgentInbox, AgentPreStep, AgentPreempt,
    AgentStatusChanged, AgentStep, AgentWake, AgentWakeEnd, AgentWakeStopping, Continuation, Phase,
    PreStep, PreStepDecision, Preempt, StatusChange, StepEvent, WakeEnded, WakeEvent, WakeStopping,
};
pub use factory::{AgentCell, AgentDriver, AgentFactory, Attach, ClaimSelector};
pub use ids::{AgentId, MessageId, SessionId, WorkerId};
pub use mail::{ClaimedMessage, Inbox, InboxReceipt, MailClass, Message, Sender, Target};

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
    /// WP-2 fills these in; named so the shape is visible in the scaffold.
    _live: parking_lot::Mutex<Vec<Agent>>,
    _factory: parking_lot::Mutex<Option<Arc<dyn AgentFactory>>>,
}

impl AgentsHandle {
    /// An empty registry with no factory. WP-2.
    pub fn new() -> AgentsHandle {
        AgentsHandle(Arc::new(AgentsInner {
            _live: parking_lot::Mutex::new(Vec::new()),
            _factory: parking_lot::Mutex::new(None),
        }))
    }

    /// §2: errors if one is already set, naming the driver that holds it. The token is an EFFECT,
    /// so unloading the driver row frees the slot and another loop Provider can take it — which
    /// is exactly what the phase's swap test does.
    ///
    /// WP-2.
    pub async fn set_factory(
        &self,
        _ctx: &Context,
        _f: Arc<dyn AgentFactory>,
    ) -> Result<EffectHandle, AgentError> {
        todo!("WP-2: take the slot, register the inverse that frees it")
    }

    /// The factory, if one is set. WP-2.
    pub fn factory(&self) -> Option<Arc<dyn AgentFactory>> {
        todo!("WP-2")
    }

    /// The creation transaction of §2: session → agent → scope → `setup` → registry →
    /// `agent/created`, with full rollback on a `setup` failure.
    ///
    /// WP-2.
    pub async fn create(&self, _req: CreateAgent) -> Result<(Agent, AgentDisposer), AgentError> {
        todo!("WP-2: the creation transaction")
    }

    /// Resume an agent that already has a row and a chain: the inbox is rebuilt from
    /// `inbox/spliced` (P2-D8) and the factory attaches with [`Attach::Resumed`].
    ///
    /// WP-2.
    pub async fn resume(&self, _req: ResumeAgent) -> Result<(Agent, AgentDisposer), AgentError> {
        todo!("WP-2: rebuild the inbox from the ledger, then attach")
    }

    /// WP-2.
    pub fn get(&self, _id: &AgentId) -> Option<Agent> {
        todo!("WP-2")
    }
    /// WP-2.
    pub fn by_name(&self, _name: &bough_plugin_ledger::AgentName) -> Option<Agent> {
        todo!("WP-2")
    }
    /// WP-2.
    pub fn list(&self) -> Vec<Agent> {
        todo!("WP-2")
    }
    /// The explicit defaulting step (§0.2): never a `?? default` inside `create`.
    ///
    /// WP-2.
    pub fn resolve_create(&self, _req: &CreateAgent) -> CreateSpec {
        todo!("WP-2: scope defaults to agent:<name>")
    }
}

impl Default for AgentsHandle {
    fn default() -> Self {
        AgentsHandle::new()
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

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-2: declare the four step types, provide::<Agents>, record the invariant stream")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::agent_lifecycle_is_sane()]
    }
}

bough_kernel::register_plugin!(AgentsPlugin);
