//! Invariant (§2): the handle IS the agent. Status never repeats, the first cancel cause wins, a
//! cancel with nothing active arms nothing, and `Disposed` never latches a pending wake. Those
//! four are enforced here, at the setters, not merely observed by the invariant module (P2-D9).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bough_kernel::{Context, ScopeKey};
use bough_plugin_ledger::{AgentName, LedgerHandle, TrajId};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::events::{AgentInbox, AgentStatusChanged, StatusChange};
use crate::factory::AgentDriver;
use crate::ids::{AgentId, SessionId};
use crate::mail::{Delivery, Inbox, InboxReceipt, MailClass, Message, Target};
use bough_plugin_llm::WakeKind;

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

/// Everything one live agent owns. Private: the handle is the only surface.
pub struct AgentInner {
    pub(crate) id: AgentId,
    pub(crate) name: AgentName,
    pub(crate) kind: AgentKind,
    pub(crate) session: Session,
    pub(crate) inbox: Inbox,
    pub(crate) ledger: LedgerHandle,
    /// The plugin's own context: where `agent/*` events are dispatched from.
    pub(crate) base: Context,
    /// The agent's SCOPE: everything registered through it unwinds with the agent.
    pub(crate) ctx: Context,
    pub(crate) scope_key: ScopeKey,
    pub(crate) status: Mutex<Status>,
    pub(crate) cancelled: Mutex<Option<CancelCause>>,
    /// Fresh on every wake: a cancelled token stays cancelled, so reusing one would make the
    /// NEXT wake start already-cancelled.
    pub(crate) token: Mutex<CancellationToken>,
    pub(crate) disposed: AtomicBool,
    /// A wake the seam has handed to the driver that has not started yet. `when_idle` waits for
    /// it, and a `Disposed` cancel clears it and never lets another be armed (§2).
    pub(crate) pending_wake: AtomicBool,
    pub(crate) driver: Mutex<Option<Arc<dyn AgentDriver>>>,
    pub(crate) idle: tokio::sync::Notify,
}

impl Agent {
    pub fn id(&self) -> &AgentId {
        &self.0.id
    }
    pub fn name(&self) -> &AgentName {
        &self.0.name
    }
    pub fn kind(&self) -> AgentKind {
        self.0.kind
    }
    pub fn session(&self) -> &Session {
        &self.0.session
    }
    pub fn traj(&self) -> &TrajId {
        &self.0.session.traj
    }
    pub fn inbox(&self) -> &Inbox {
        &self.0.inbox
    }
    pub fn status(&self) -> Status {
        *self.0.status.lock()
    }
    /// The agent's SCOPE (§5): scoped tools, sections and `tools.restrict` register through it
    /// and unwind with the agent.
    pub fn ctx(&self) -> &Context {
        &self.0.ctx
    }
    pub fn scope_key(&self) -> &ScopeKey {
        &self.0.scope_key
    }
    /// The ledger this agent's chain lives in.
    pub fn ledger(&self) -> &LedgerHandle {
        &self.0.ledger
    }
    /// Whether teardown (or a `Disposed` cancel) has run.
    pub fn is_disposed(&self) -> bool {
        self.0.disposed.load(Ordering::SeqCst)
    }
    /// Whether a wake has been handed to the driver and has not started.
    /// Arm the pending wake without a `send`: the creation transaction's seed mail takes this
    /// path, because the driver does not exist yet when the seed is spliced.
    pub(crate) fn arm_pending_wake(&self) {
        self.0.pending_wake.store(true, Ordering::SeqCst);
    }
    pub(crate) fn clear_pending_wake(&self) {
        self.0.pending_wake.store(false, Ordering::SeqCst);
    }
    pub fn has_pending_wake(&self) -> bool {
        self.0.pending_wake.load(Ordering::SeqCst)
    }
    /// The driver, once the factory has attached one.
    pub fn driver(&self) -> Option<Arc<dyn AgentDriver>> {
        self.0.driver.lock().clone()
    }
    /// The token every cancel cause fires.
    pub fn cancel_token(&self) -> CancellationToken {
        self.0.token.lock().clone()
    }

    /// First cause wins; nothing active ⇒ a no-op that never arms later work; `Disposed` never
    /// latches a pending wake (§2).
    pub async fn cancel(&self, cause: CancelCause, keep_inbox: bool) {
        if cause == CancelCause::Disposed {
            // Terminal, whatever the status: after this no wake can be armed, and any wake the
            // seam had already handed to the driver is un-latched.
            self.0.disposed.store(true, Ordering::SeqCst);
            self.0.pending_wake.store(false, Ordering::SeqCst);
            self.0.token.lock().cancel();
            self.0.idle.notify_waiters();
        }
        if self.status() != Status::Running {
            // Nothing active: a no-op. In particular it does not record a cause that a LATER
            // wake would then observe, and it never touches the inbox.
            return;
        }
        {
            let mut held = self.0.cancelled.lock();
            if held.is_some() {
                // First cause wins.
                return;
            }
            *held = Some(cause);
        }
        self.0.token.lock().cancel();
        let driver = self.driver();
        if let Some(driver) = driver {
            driver.cancel(cause, keep_inbox).await;
        }
    }

    /// The cause that won, if any.
    pub fn cancelled_by(&self) -> Option<CancelCause> {
        *self.0.cancelled.lock()
    }

    /// Resolves when the agent is idle and no wake is scheduled.
    pub async fn when_idle(&self) {
        loop {
            let notified = self.0.idle.notified();
            if self.status() == Status::Idle && !self.has_pending_wake() {
                return;
            }
            notified.await;
        }
    }

    /// Every inbox mutation is a durable `inbox/spliced` step keyed by the message id (§2).
    pub async fn send(
        &self,
        msg: Message,
        target: Target,
        wake: bool,
    ) -> Result<InboxReceipt, AgentError> {
        if self.is_disposed() {
            return Err(AgentError::Disposed {
                name: self.0.name.clone(),
            });
        }
        let receipt = self
            .0
            .inbox
            .insert_waking(msg.clone(), target, wake)
            .await?;
        if wake {
            self.0.pending_wake.store(true, Ordering::SeqCst);
            // §2: "disposed never latches a pending wake". The disposal check above and this
            // store are not one atom — a `cancel(Disposed)` landing between them used to set
            // `disposed` and clear the flag while this call was still on its way to setting it,
            // leaving a disposed agent latched forever (nothing clears it once the driver is
            // detached, so `when_idle()` never returned). Re-reading the flag AFTER the store
            // closes the window from this side; the disposer's own clear closes it from the
            // other, and whichever runs last leaves the flag down.
            if self.is_disposed() {
                self.0.pending_wake.store(false, Ordering::SeqCst);
                self.0.idle.notify_waiters();
            }
        }
        self.0
            .base
            .emit::<AgentInbox>((receipt.clone(), msg.clone()));
        if let Some(driver) = self.driver() {
            driver.notify(&receipt, &msg).await;
        }
        Ok(receipt)
    }

    /// Preset: `NextWake`, wake.
    pub async fn followup(&self, msg: Message) -> Result<InboxReceipt, AgentError> {
        self.send(msg, Target::NextWake, true).await
    }
    /// Preset: `NextStep`, wake.
    pub async fn steer(&self, msg: Message) -> Result<InboxReceipt, AgentError> {
        self.send(msg, Target::NextStep, true).await
    }
    /// Preset: `NextStep`, no wake.
    pub async fn inject(&self, msg: Message) -> Result<InboxReceipt, AgentError> {
        self.send(msg, Target::NextStep, false).await
    }

    /// §5's catch-up / schedule entry point (P3-D16). Opens ONE wake of `kind` if there is
    /// anything to process, and does nothing at all otherwise. Never appends a synthetic message:
    /// the driver already knows whether there is work, so asking it is one method — and it is the
    /// same method Phase 7's `sleep-listener` needs.
    pub async fn request_wake(&self, kind: WakeKind, cause: WakeCause) -> WakeRequest {
        // A disposed agent is terminal (§2): nothing may arm work on it, and asking a driver that
        // is already stopping would be exactly that.
        if self.is_disposed() {
            return WakeRequest::Nothing;
        }
        match self.driver() {
            // No driver yet is not an error and not a wake: the factory has not attached, so
            // there is nothing that could run.
            None => WakeRequest::Nothing,
            Some(driver) => driver.wake_now(kind, cause).await,
        }
    }

    /// DELIVERED mail (§3, §5): appends `mail/delivered` (EVIDENCE, cited) and then splices the
    /// message carrying that step's seq, so the pair can never be half-written by a producer
    /// (P3-D15). This is what Phase 6's collectors will use; the old-feed adapter is its first
    /// caller.
    pub async fn deliver(&self, mail: Delivery) -> Result<InboxReceipt, AgentError> {
        if self.is_disposed() {
            return Err(AgentError::Disposed {
                name: self.0.name.clone(),
            });
        }
        // ORDER IS THE POINT (P3-D15). The `mail/delivered` step goes first, because the splice
        // has to CARRY its seq: §5's consumption is per (agent, mail seq), and a message spliced
        // without one would never be consumable. The step is EVIDENCE, so a delivery that cannot
        // say where it came from is refused by the ledger rather than by a rule written here.
        let body = serde_json::to_value(bough_plugin_ledger::vocabulary::MailDelivered {
            class: mail.class,
            from: bough_plugin_ledger::Ref::new(mail.from.as_ref_str()),
            subject: mail.subject.clone(),
            summary: mail.summary.clone(),
            refs: mail.refs.iter().cloned().collect(),
        })
        .expect("MailDelivered serializes");
        let step = self
            .0
            .ledger
            .0
            .append(bough_plugin_ledger::Append {
                traj: self.0.session.traj.clone(),
                wake: crate::mail::outside_wake(),
                kind: bough_plugin_ledger::StepType::new("mail/delivered"),
                class: bough_plugin_ledger::Class::Evidence,
                body,
                cites: mail.cites.clone(),
                at: mail.at,
                id: None,
            })
            .await?;

        let msg = Message {
            id: crate::ids::MessageId::new(uuid::Uuid::now_v7().to_string()),
            from: mail.from,
            class: mail.class,
            text: mail.text,
            subject: mail.subject,
            cites: mail.cites,
            refs: mail.refs,
            // The seq of the step appended a moment ago: the pair is what makes delivered mail
            // consumable, and neither half is written without the other.
            mail_seq: Some(step.seq),
            at: mail.at,
        };
        // Wake-class mail is itself a wake reason (§5); ordinary delivered mail waits for a drain.
        let wake = mail.class == MailClass::Wake;
        self.send(msg, Target::NextWake, wake).await
    }

    /// Publish a status transition. `AgentCell::set_status` is the only caller.
    pub(crate) fn set_status(&self, to: Status) -> Result<(), AgentError> {
        let from = {
            let mut held = self.0.status.lock();
            if *held == to {
                return Err(AgentError::StatusRepeat(to));
            }
            let from = *held;
            *held = to;
            from
        };
        if to == Status::Running {
            // The pending wake has become a running one.
            self.0.pending_wake.store(false, Ordering::SeqCst);
            if !self.is_disposed() {
                *self.0.token.lock() = CancellationToken::new();
            }
        } else {
            // A finished wake clears the cause, so the NEXT wake starts uncancelled.
            *self.0.cancelled.lock() = None;
        }
        self.0.idle.notify_waiters();
        self.0.base.emit::<AgentStatusChanged>(StatusChange {
            agent: self.0.id.clone(),
            from,
            to,
        });
        Ok(())
    }
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Agent({:?})", self.0.name)
    }
}

/// Why a wake was asked for. Attribution for `request_wake`, never authorization.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WakeCause {
    /// §5's lid-open catch-up. TUI launch is the proxy until Phase 7's `sleep-listener`.
    CatchUp,
    /// A schedule fired; the `&'static str` names it.
    Schedule(&'static str),
}

/// What [`Agent::request_wake`] did. `Nothing` is the answer for an agent with nothing queued —
/// V6's "and none when nothing is queued".
#[derive(Clone, Debug, PartialEq)]
pub enum WakeRequest {
    Started(bough_plugin_ledger::WakeId),
    Nothing,
}
