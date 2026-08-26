//! Invariant (§2): creation is a TRANSACTION. Private session, concrete agent, scoped context,
//! optional `setup(agent_ctx)`, registry entry, `agent/created` — and a `setup` failure rolls
//! every one of those back. The returned disposer is a CAPABILITY: only its holder can tear the
//! agent down, and teardown runs stop+drain → unwind scope → detach agent → detach session.

use std::sync::Arc;

use bough_kernel::{EffectHandle, ScopeKey};
use bough_plugin_ledger::{AgentName, TrajId};
use chrono::{DateTime, Utc};

use crate::agent::{Agent, AgentKind, CancelCause};
use crate::error::AgentError;
use crate::events::AgentDisposed;
use crate::mail::{Message, Target};
use crate::{trace, AgentsHandle};

/// A creation request.
pub struct CreateAgent {
    pub name: AgentName,
    pub traj: TrajId,
    pub kind: AgentKind,
    /// `None` ⇒ `ScopeKey::new(format!("agent:{name}"))`, filled in by
    /// [`crate::AgentsHandle::resolve_create`].
    pub scope: Option<ScopeKey>,
    pub setup: Option<Arc<dyn AgentSetup>>,
    /// Mail the agent starts with, spliced inside the same transaction.
    pub seed: Vec<(Message, Target)>,
    pub at: DateTime<Utc>,
}

impl CreateAgent {
    /// The minimal request: a resident agent on `traj`, default scope, no setup, no seed.
    pub fn resident(name: AgentName, traj: TrajId, at: DateTime<Utc>) -> CreateAgent {
        CreateAgent {
            name,
            traj,
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at,
        }
    }
}

/// What a [`CreateAgent`] resolves to before anything is created (§0.2: defaulting is explicit).
#[derive(Clone, Debug, PartialEq)]
pub struct CreateSpec {
    pub name: AgentName,
    pub traj: TrajId,
    pub kind: AgentKind,
    pub scope: ScopeKey,
}

/// A resume request: the agent's row and chain already exist.
pub struct ResumeAgent {
    pub name: AgentName,
    pub at: DateTime<Utc>,
    pub setup: Option<Arc<dyn AgentSetup>>,
}

/// Work that runs INSIDE the creation transaction.
#[async_trait::async_trait]
pub trait AgentSetup: Send + Sync + 'static {
    /// Runs while BOTH ids are still unpublished (§2). An `Err` rolls the whole creation back.
    async fn setup(&self, agent: &Agent) -> Result<(), AgentError>;
}

/// The teardown capability of §2. Deliberately not `Clone`.
pub struct AgentDisposer {
    pub(crate) agent: Agent,
    pub(crate) scope: EffectHandle,
    pub(crate) agents: AgentsHandle,
}

impl AgentDisposer {
    /// Teardown order, normative: stop and drain → unwind scope → detach agent → detach session.
    pub async fn dispose(self) {
        let id = self.agent.id().clone();

        // 1. stop and drain: no new wake starts, the in-flight wake ends.
        if let Some(driver) = self.agent.driver() {
            driver.stop().await;
        }
        trace::push(&id, "stop");

        // 2. unwind the scope: everything a plugin registered through `agent.ctx()` goes.
        self.scope.dispose().await;
        trace::push(&id, "scope");

        // 3. detach the agent: out of the registry, terminally cancelled. `Disposed` never
        //    latches a pending wake (§2), which `Agent::cancel` enforces.
        self.agents.detach(&id);
        self.agent.cancel(CancelCause::Disposed, false).await;
        trace::push(&id, "agent");

        // 4. detach the session: the driver slot is released and the live queues are dropped.
        //    The DURABLE chain is untouched — an agent's trajectory outlives its handle.
        *self.agent.0.driver.lock() = None;
        self.agent.inbox().clear_live();
        trace::push(&id, "session");

        self.agent.0.base.emit::<AgentDisposed>(id);
    }

    /// The agent this disposer owns.
    pub fn agent(&self) -> &Agent {
        &self.agent
    }
}

impl std::fmt::Debug for AgentDisposer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AgentDisposer({:?})", self.agent.name())
    }
}
