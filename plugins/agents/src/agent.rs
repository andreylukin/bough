//! Invariant (§2): the handle IS the agent. Status never repeats, the first cancel cause wins, a
//! cancel with nothing active arms nothing, and `Disposed` never latches a pending wake. Those
//! four are enforced here, at the setters, not merely observed by the invariant module (P2-D9).

use std::sync::Arc;

use bough_kernel::{Context, ScopeKey};
use bough_plugin_ledger::{AgentName, TrajId};
use chrono::{DateTime, Utc};

use crate::error::AgentError;
use crate::ids::{AgentId, SessionId};
use crate::mail::{Inbox, InboxReceipt, Message, Target};

/// Whether the agent is inside a wake.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Idle,
    Running,
}

/// §2's typed cancellation causes. First cause wins.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum CancelCause {
    /// Andrey pressed stop.
    User,
    /// The spawner cancelled a worker.
    Parent,
    /// A plugin cancelled from `agent/wake-stopping`.
    Hook,
    /// The disposer ran. Never latches a pending wake (§2).
    Disposed,
}

/// What kind of agent this is. Phase 2 exercises one resident; the other two are Phase 5's.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Resident,
    Worker,
    Fork,
}

/// An agent's private session.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub id: SessionId,
    pub traj: TrajId,
    pub created_at: DateTime<Utc>,
}

/// The live agent handle of §2, verbatim in shape.
#[derive(Clone)]
pub struct Agent(pub(crate) Arc<AgentInner>);

/// Everything one live agent owns. Private: the handle is the only surface (WP-2 fills it in).
pub struct AgentInner {
    pub(crate) _id: AgentId,
    pub(crate) _name: AgentName,
}

impl Agent {
    /// WP-2.
    pub fn id(&self) -> &AgentId {
        todo!("WP-2")
    }
    /// WP-2.
    pub fn name(&self) -> &AgentName {
        todo!("WP-2")
    }
    /// WP-2.
    pub fn kind(&self) -> AgentKind {
        todo!("WP-2")
    }
    /// WP-2.
    pub fn session(&self) -> &Session {
        todo!("WP-2")
    }
    /// WP-2.
    pub fn traj(&self) -> &TrajId {
        todo!("WP-2")
    }
    /// WP-2.
    pub fn inbox(&self) -> &Inbox {
        todo!("WP-2")
    }
    /// WP-2.
    pub fn status(&self) -> Status {
        todo!("WP-2")
    }
    /// The agent's SCOPE (§5): scoped tools, sections and `tools.restrict` register through it
    /// and unwind with the agent.
    ///
    /// WP-2.
    pub fn ctx(&self) -> &Context {
        todo!("WP-2")
    }
    /// WP-2.
    pub fn scope_key(&self) -> &ScopeKey {
        todo!("WP-2")
    }
    /// First cause wins; nothing active ⇒ a no-op that never arms later work; `Disposed` never
    /// latches a pending wake (§2).
    ///
    /// WP-2.
    pub async fn cancel(&self, _cause: CancelCause, _keep_inbox: bool) {
        todo!("WP-2: first-wins cancellation")
    }
    /// The cause that won, if any. WP-2.
    pub fn cancelled_by(&self) -> Option<CancelCause> {
        todo!("WP-2")
    }
    /// Resolves when the agent is idle and no wake is scheduled. WP-2.
    pub async fn when_idle(&self) {
        todo!("WP-2")
    }
    /// Every inbox mutation is a durable `inbox/spliced` step keyed by the message id (§2).
    ///
    /// WP-2.
    pub async fn send(
        &self,
        _msg: Message,
        _target: Target,
        _wake: bool,
    ) -> Result<InboxReceipt, AgentError> {
        todo!("WP-2: splice, notify the driver, return the receipt")
    }
    /// Preset: `NextWake`, wake. WP-2.
    pub async fn followup(&self, _msg: Message) -> Result<InboxReceipt, AgentError> {
        todo!("WP-2: send(msg, Target::NextWake, true)")
    }
    /// Preset: `NextStep`, wake. WP-2.
    pub async fn steer(&self, _msg: Message) -> Result<InboxReceipt, AgentError> {
        todo!("WP-2: send(msg, Target::NextStep, true)")
    }
    /// Preset: `NextStep`, no wake. WP-2.
    pub async fn inject(&self, _msg: Message) -> Result<InboxReceipt, AgentError> {
        todo!("WP-2: send(msg, Target::NextStep, false)")
    }
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Agent({:?})", self.0._name)
    }
}
