//! Invariant (§2): there is at most ONE agent factory, and it is a slot an effect holds — so
//! unloading the driver row frees it and another loop Provider can take it. That is what makes
//! the phase's swap test possible without a recompile.

use std::sync::Arc;

use bough_plugin_ledger::vocabulary::SpliceOp;
use bough_plugin_ledger::LedgerHandle;
use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, CancelCause, Status, WakeCause, WakeRequest};
use crate::error::AgentError;
use crate::ids::MessageId;
use crate::mail::{ClaimedMessage, InboxReceipt, MailClass, Message, Target};
use bough_plugin_ledger::WakeId;
use bough_plugin_llm::WakeKind;

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
    /// §5's catch-up entry point (P3-D16). Both drivers implement it. `agent-loop`: `Nothing`
    /// unless `pending(NextWake)` is non-empty or unconsumed ordinary mail exists; otherwise one
    /// wake with the oldest queued item as trigger. `agent-loop-scripted`: one scripted wake if
    /// the transcript has one left.
    async fn wake_now(&self, kind: WakeKind, cause: WakeCause) -> WakeRequest;
}

/// The driver's private view of an agent: the only way to publish status or claim inbox items.
pub struct AgentCell {
    pub(crate) agent: Agent,
}

impl AgentCell {
    pub fn agent(&self) -> &Agent {
        &self.agent
    }
    pub fn ledger(&self) -> &LedgerHandle {
        self.agent.ledger()
    }
    /// Refuses a repeat (`Running → Running`): the invariant is enforced at the setter, not only
    /// observed (P2-D9). Emits `agent/status`.
    pub async fn set_status(&self, to: Status) -> Result<(), AgentError> {
        self.agent.set_status(to)
    }
    /// Every wake a driver starts says so here, whatever it does about `status`.
    ///
    /// The pending-wake flag means "mail is armed and no wake has picked it up yet". It used to
    /// be cleared as a SIDE EFFECT of the `Idle -> Running` status edge, which stopped being one
    /// edge per wake the moment `status` became the driver-wide interval §2 says it is: a second
    /// concurrent wake published no edge, so nothing cleared the flag and `when_idle()` never
    /// returned.
    pub fn wake_started(&self) {
        self.agent.clear_pending_wake();
    }

    /// The other outcome of an armed wake: `agent/wake-request` refused it, so no wake will start
    /// and nothing else would ever lower the flag (P5-D1). Idle waiters are notified, because an
    /// agent whose only pending wake was refused IS idle.
    pub fn wake_refused(&self) {
        self.agent.refuse_pending_wake();
    }

    /// A pure DELETION splice (§5): appends one `inbox/spliced { op: claim }` per message.
    pub async fn claim(
        &self,
        sel: ClaimSelector,
        wake: WakeId,
        at: DateTime<Utc>,
    ) -> Result<Vec<ClaimedMessage>, AgentError> {
        // §5 runs an answer wake CONCURRENTLY with the wake it interrupted, and both claim the
        // `next-step` queue. Selecting and removing under two separate locks let both wakes take
        // the same message and deliver it to the model twice, so the take is ONE critical
        // section: whoever wins the lock owns the message, and the durable splice follows.
        let chosen = self.agent.inbox().take(&sel);
        let mut out = Vec::with_capacity(chosen.len());
        for message in chosen {
            let claim_step = self
                .agent
                .inbox()
                .append_removal(
                    &message.id,
                    sel.target,
                    SpliceOp::Claim,
                    wake.clone(),
                    at,
                    None,
                )
                .await?;
            out.push(ClaimedMessage {
                message,
                target: sel.target,
                claim_step,
            });
        }
        Ok(out)
    }
    /// Drop a message without delivering it, durably.
    pub async fn discard(
        &self,
        id: &MessageId,
        wake: WakeId,
        reason: &str,
        at: DateTime<Utc>,
    ) -> Result<(), AgentError> {
        let in_next_step = self
            .agent
            .inbox()
            .pending(Target::NextStep)
            .iter()
            .any(|m| m.id == *id);
        let target = if in_next_step {
            Target::NextStep
        } else {
            Target::NextWake
        };
        self.agent
            .inbox()
            .remove(id, target, SpliceOp::Discard, wake, at, Some(reason))
            .await?;
        Ok(())
    }
    /// The token every cancel cause fires.
    pub fn cancel_token(&self) -> CancellationToken {
        self.agent.cancel_token()
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
    /// §5: "an Andrey message ALWAYS gets a fresh sol answer wake … drain and tick wakes never
    /// answer him". Class alone cannot express that on the `next-step` queue, where the class
    /// filter does not apply, so the exclusion is its own field.
    pub exclude_andrey: bool,
    pub limit: Option<usize>,
}

impl ClaimSelector {
    /// Everything queued for `target`.
    pub fn all(target: Target) -> ClaimSelector {
        ClaimSelector {
            target,
            only: None,
            classes: None,
            exclude_andrey: false,
            limit: None,
        }
    }
}
