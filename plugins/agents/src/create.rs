//! Invariant (§2): creation is a TRANSACTION. Private session, concrete agent, scoped context,
//! optional `setup(agent_ctx)`, registry entry, `agent/created` — and a `setup` failure rolls
//! every one of those back. The returned disposer is a CAPABILITY: only its holder can tear the
//! agent down, and teardown runs stop+drain → unwind scope → detach agent → detach session.

use std::sync::Arc;

use bough_kernel::ScopeKey;
use bough_plugin_ledger::{AgentName, TrajId};
use chrono::{DateTime, Utc};

use crate::agent::{Agent, AgentKind};
use crate::error::AgentError;
use crate::mail::{Message, Target};

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
    /// WP-2 fills this in.
    pub(crate) _agent: Agent,
}

impl AgentDisposer {
    /// Teardown order, normative: stop and drain → unwind scope → detach agent → detach session.
    ///
    /// WP-2.
    pub async fn dispose(self) {
        todo!("WP-2: the four-stage teardown, in order")
    }

    /// The agent this disposer owns. WP-2.
    pub fn agent(&self) -> &Agent {
        &self._agent
    }
}
