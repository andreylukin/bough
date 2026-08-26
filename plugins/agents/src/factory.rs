//! Invariant (§2): there is at most ONE agent factory, and it is a slot an effect holds — so
//! unloading the driver row frees it and another loop Provider can take it. That is what makes
//! the phase's swap test possible without a recompile.

use std::sync::Arc;

use bough_plugin_ledger::LedgerHandle;
use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, CancelCause, Status};
use crate::error::AgentError;
use crate::ids::MessageId;
use crate::mail::{ClaimedMessage, InboxReceipt, MailClass, Message, Target};
use bough_plugin_ledger::WakeId;

/// Whether the driver is attaching to a fresh agent or a resumed one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Attach {
    Created,
    Resumed,
}

/// What a loop Provider registers.
#[async_trait::async_trait]
pub trait AgentFactory: Send + Sync + 'static {
    /// The catalog name of the loop behind this factory; the swap test reads it.
    fn driver(&self) -> &'static str;
    /// The session, the scope and the handle exist; the registry entry does not yet.
    async fn attach(
        &self,
        cell: AgentCell,
        mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError>;
}

/// One agent's running loop, from the seam's side.
#[async_trait::async_trait]
pub trait AgentDriver: Send + Sync + 'static {
    fn driver(&self) -> &'static str;
    /// A durable inbox mutation landed: schedule (or not) per target, wake flag and urgency.
    async fn notify(&self, receipt: &InboxReceipt, msg: &Message);
    async fn cancel(&self, cause: CancelCause, keep_inbox: bool);
    /// Stop and drain: no new wake starts, the in-flight wake ends, returns when idle.
    async fn stop(&self);
}

/// The driver's private view of an agent: the only way to publish status or claim inbox items.
pub struct AgentCell {
    /// WP-2 fills this in.
    pub(crate) _agent: Agent,
}

impl AgentCell {
    /// WP-2.
    pub fn agent(&self) -> &Agent {
        &self._agent
    }
    /// WP-2.
    pub fn ledger(&self) -> &LedgerHandle {
        todo!("WP-2")
    }
    /// Refuses a repeat (`Running → Running`): the invariant is enforced at the setter, not only
    /// observed (P2-D9). Emits `agent/status`.
    ///
    /// WP-2.
    pub async fn set_status(&self, _to: Status) -> Result<(), AgentError> {
        todo!("WP-2: refuse a repeat, then emit agent/status")
    }
    /// A pure DELETION splice (§5): appends one `inbox/spliced { op: claim }` per message.
    ///
    /// WP-2.
    pub async fn claim(
        &self,
        _sel: ClaimSelector,
        _wake: WakeId,
        _at: DateTime<Utc>,
    ) -> Result<Vec<ClaimedMessage>, AgentError> {
        todo!("WP-2: durable claim splice, then remove from the live cache")
    }
    /// Drop a message without delivering it, durably. WP-2.
    pub async fn discard(
        &self,
        _id: &MessageId,
        _wake: WakeId,
        _reason: &str,
        _at: DateTime<Utc>,
    ) -> Result<(), AgentError> {
        todo!("WP-2: inbox/spliced with op discard")
    }
    /// The token every cancel cause fires. WP-2.
    pub fn cancel_token(&self) -> CancellationToken {
        todo!("WP-2")
    }
}

/// Which inbox items a claim takes.
#[derive(Clone, Debug)]
pub struct ClaimSelector {
    pub target: Target,
    /// Exactly these messages, in this order. `None` ⇒ everything the other filters admit.
    pub only: Option<Vec<MessageId>>,
    /// A drain wake claims ORDINARY seqs only (§5); an answer wake claims its trigger only.
    pub classes: Option<Vec<MailClass>>,
    pub limit: Option<usize>,
}
