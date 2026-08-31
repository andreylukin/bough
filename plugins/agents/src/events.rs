//! Invariant (§0.2, P2-D25): every EMIT event here is a LIVE MIRROR of a fact that is already
//! durable. Nothing in this phase reads an emit to decide anything — `emit` dispatch is spawned,
//! not awaited, so a durable fact rides a ledger step and never an event.

use bough_kernel::{EmitEvent, ParallelEvent, SerialEvent, WaterfallEvent};
use bough_plugin_ledger::vocabulary::WakeEndReason;
use bough_plugin_ledger::{AgentName, StepId, WakeId};
use bough_plugin_llm::{LlmMessage, WakeKind};

use crate::agent::{Agent, Status};
use crate::ids::{AgentId, MessageId};
use crate::mail::{ClaimedMessage, InboxReceipt, Message};

/// `agent/created` — the creation transaction committed.
pub struct AgentCreated;
impl EmitEvent for AgentCreated {
    const NAME: &'static str = "agent/created";
    type Payload = Agent;
}

/// `agent/disposed` — teardown finished.
pub struct AgentDisposed;
impl EmitEvent for AgentDisposed {
    const NAME: &'static str = "agent/disposed";
    type Payload = AgentId;
}

/// What a structural op did to the `agents` ROWS. §3 makes the rows MUTABLE CONFIG, and the live
/// registry is a separate thing: an `Agent`'s trajectory is immutable for its life, so a row whose
/// `traj` moved under a merge, or a row born by a split, needs the LIVE half brought back into
/// line with it. That reconciliation is not the registry's job — it does not own the disposers —
/// so the fact is published and the row that owns liveness (`residents`) acts on it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowsChanged {
    pub written: Vec<AgentName>,
    pub deleted: Vec<AgentName>,
}

/// `agents/rows-changed` — EMIT. Published by `graph-ops` after every op that writes or deletes an
/// `agents` row.
pub struct AgentRowsChanged;
impl EmitEvent for AgentRowsChanged {
    const NAME: &'static str = "agents/rows-changed";
    type Payload = RowsChanged;
}

/// A status transition. Never a repeat (P2-D9).
#[derive(Clone, Debug, PartialEq)]
pub struct StatusChange {
    pub agent: AgentId,
    pub from: Status,
    pub to: Status,
}

/// `agent/status`.
pub struct AgentStatusChanged;
impl EmitEvent for AgentStatusChanged {
    const NAME: &'static str = "agent/status";
    type Payload = StatusChange;
}

/// `agent/inbox` — a durable splice landed.
pub struct AgentInbox;
impl EmitEvent for AgentInbox {
    const NAME: &'static str = "agent/inbox";
    type Payload = (InboxReceipt, Message);
}

/// Which half of a wake or step this event mirrors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Start,
    End,
}

/// The live mirror of `wake/start` / `wake/end`. The DURABLE fact is the step.
#[derive(Clone, Debug)]
pub struct WakeEvent {
    pub agent: AgentId,
    pub wake: WakeId,
    pub kind: WakeKind,
    pub phase: Phase,
}

/// `agent/wake`.
pub struct AgentWake;
impl EmitEvent for AgentWake {
    const NAME: &'static str = "agent/wake";
    type Payload = WakeEvent;
}

/// The live mirror of `step/start` / `step/end`.
#[derive(Clone, Debug)]
pub struct StepEvent {
    pub agent: AgentId,
    pub wake: WakeId,
    pub index: u32,
    pub phase: Phase,
}

/// `agent/step`.
pub struct AgentStep;
impl EmitEvent for AgentStep {
    const NAME: &'static str = "agent/step";
    type Payload = StepEvent;
}

/// The value of the `agent/pre-step` waterfall.
#[derive(Clone, Debug)]
pub struct PreStep {
    pub agent: AgentId,
    pub name: AgentName,
    pub wake: WakeId,
    pub kind: WakeKind,
    pub step_index: u32,
    /// Read-only: the claim is ALREADY durable when the chain runs.
    pub claimed: Vec<ClaimedMessage>,
    pub decision: PreStepDecision,
}

/// What the chain decided about this step.
#[derive(Clone, Debug, PartialEq)]
pub enum PreStepDecision {
    /// Messages the model will see for this step. Claimed messages the decision omits STAY
    /// REMOVED (§5): they are already spliced out and are the omitter's problem.
    Enter { messages: Vec<LlmMessage> },
    /// No step runs. The wake still closes durably, with reason `completed`.
    Reject { reason: String },
}

/// `agent/pre-step` — §5: reject | enter(messages).
pub struct AgentPreStep;
impl WaterfallEvent for AgentPreStep {
    const NAME: &'static str = "agent/pre-step";
    type Value = PreStep;
}

/// The payload of `agent/wake-stopping`.
#[derive(Clone)]
pub struct WakeStopping {
    pub agent: AgentId,
    pub wake: WakeId,
    pub kind: WakeKind,
    pub steps: u32,
    /// Whether a tool result already concluded the wake.
    pub concludes: bool,
    /// The live handle, so a listener that wants to bound a runaway wake can cancel (§5).
    pub handle: Agent,
}

/// `agent/wake-stopping` — SERIAL with an uninhabited output (P2-D10): every listener runs, in
/// order, and the decision is read from the inbox afterwards, so listener order cannot change it.
pub struct AgentWakeStopping;
impl SerialEvent for AgentWakeStopping {
    const NAME: &'static str = "agent/wake-stopping";
    type Payload = WakeStopping;
    type Output = std::convert::Infallible;
}

/// The payload of `agent/wake-end`.
#[derive(Clone, Debug)]
pub struct WakeEnded {
    pub agent: AgentId,
    pub wake: WakeId,
    pub reason: WakeEndReason,
    /// A short human summary the loop already has; listeners may ignore it.
    pub summary: String,
    /// The `wake/end` step itself, so a listener's own step can cite it.
    pub end_step: StepId,
}

/// `agent/wake-end` — PARALLEL, dispatched for COMPLETED wakes ONLY. Where the about-line refresh
/// happens (P2-D11).
pub struct AgentWakeEnd;
impl ParallelEvent for AgentWakeEnd {
    const NAME: &'static str = "agent/wake-end";
    type Payload = WakeEnded;
}

/// §5's checkpoint-and-answer moment.
#[derive(Clone, Debug)]
pub struct Preempt {
    pub agent: AgentId,
    pub interrupted: WakeId,
    pub by: MessageId,
    pub answer: WakeId,
}

/// `agent/preempt`.
pub struct AgentPreempt;
impl EmitEvent for AgentPreempt {
    const NAME: &'static str = "agent/preempt";
    type Payload = Preempt;
}

/// A wake resumed from a jot.
#[derive(Clone, Debug)]
pub struct Continuation {
    pub agent: AgentId,
    pub wake: WakeId,
    pub from_jot: StepId,
}

/// `agent/continuation`.
pub struct AgentContinuation;
impl EmitEvent for AgentContinuation {
    const NAME: &'static str = "agent/continuation";
    type Payload = Continuation;
}

// ---- `agent/wake-request`: the admission point (P5-D1, §1) ---------------------------------

/// `agent/wake-request` — WATERFALL, dispatched by EVERY loop Provider immediately before it
/// opens a wake and appends `wake/start`. A listener that returns [`Admit::Defer`] stops the wake
/// from existing at all: no `wake/start`, no claim, no step. The default (no listener) is
/// [`Admit::Open`], so a tree without the `dormancy` row behaves exactly as it did in Phase 4.
///
/// This amends §5's wake flow and is flagged as such (P5-D1): `agent/pre-step` fires INSIDE an
/// already-durable wake, so suppressing there would leave a trail of empty wakes for an agent that
/// is supposed to cost nothing.
pub struct AgentWakeRequest;
impl WaterfallEvent for AgentWakeRequest {
    const NAME: &'static str = "agent/wake-request";
    type Value = WakeAdmission;
}

/// The value the admission waterfall carries.
#[derive(Clone, Debug)]
pub struct WakeAdmission {
    pub agent: AgentName,
    pub id: AgentId,
    pub kind: WakeKind,
    pub cause: crate::agent::WakeCause,
    /// What would trigger this wake, read from the inbox WITHOUT claiming it.
    pub trigger: Option<TriggerFacts>,
    pub at: chrono::DateTime<chrono::Utc>,
    pub decision: Admit,
}

/// The admission decision.
#[derive(Clone, Debug, PartialEq)]
pub enum Admit {
    /// Open the wake.
    Open,
    /// No wake exists. `by` names the row that deferred, for the toast and the ledger-free
    /// explanation.
    Defer { by: &'static str, reason: String },
}

/// The facts about the message that would open this wake, so a listener never re-reads the inbox.
#[derive(Clone, Debug, PartialEq)]
pub struct TriggerFacts {
    pub message: MessageId,
    pub from_andrey: bool,
    pub class: crate::mail::MailClass,
    /// The message's refs — P5-D3 spells a wake CLASS as a ref in the `class:` namespace.
    pub refs: std::collections::BTreeSet<bough_plugin_ledger::Ref>,
    pub mail_seq: Option<bough_plugin_ledger::Seq>,
}
