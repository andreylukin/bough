//! Invariant (§2): this is the `AgentFactory` / `AgentDriver` the `agents` seam's factory slot
//! holds. Everything the loop knows about scheduling lives behind these four methods, so a
//! replacement loop (`agent-loop-scripted`) is held to the ledger protocol and not to a feature
//! list.
//!
//! Two rules are enforced here rather than described: ONE drain wake in flight per agent (§5),
//! and one answer wake at a time — a message arriving during an answer wake joins it before the
//! first streamed token and queues after it (§5, P2-D15).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bough_plugin_agents::{
    AgentCell, AgentDriver, AgentError, AgentFactory, Attach, CancelCause, InboxReceipt, Message,
    Target,
};
use bough_plugin_ledger::{StepId, WakeId};
use bough_plugin_llm::WakeKind;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::mail::{DrainGate, Schedule};
use crate::preempt::{Preemption, Running};
use crate::wake::{LoopDeps, WakeSpec};
use crate::LoopConfig;

/// Per-FIBER live driver state the invariants read: how many drain wakes are scheduled, and which
/// wakes are open right now.
///
/// Partitioned by `FiberUid` for the same reason the reconstruction recorder is: a process-wide
/// counter let another kernel in the same test binary satisfy this tree's standing invariant
/// without checking anything.
#[derive(Default)]
struct LiveState {
    drains: usize,
    open: std::collections::BTreeSet<String>,
}

static LIVE: Mutex<std::collections::BTreeMap<bough_kernel::FiberUid, LiveState>> =
    Mutex::new(std::collections::BTreeMap::new());

fn with_live<R>(fiber: bough_kernel::FiberUid, f: impl FnOnce(&mut LiveState) -> R) -> R {
    let mut live = LIVE.lock();
    f(live.entry(fiber).or_default())
}

/// Whether a live driver of THIS fiber has a drain wake scheduled. Read by the standing-invariant
/// check.
pub fn any_drain_scheduled(fiber: bough_kernel::FiberUid) -> bool {
    LIVE.lock()
        .get(&fiber)
        .map(|l| l.drains > 0)
        .unwrap_or(false)
}

/// The wakes this fiber's drivers currently have open. `evaluate_wake_pairing` excuses exactly
/// these: a wake that is running is not a wake that was never closed.
pub fn live_wakes(fiber: bough_kernel::FiberUid) -> Vec<WakeId> {
    LIVE.lock()
        .get(&fiber)
        .map(|l| l.open.iter().map(WakeId::new).collect())
        .unwrap_or_default()
}

/// Drop everything recorded for `fiber` (registered as an inverse by `apply`).
pub fn forget(fiber: bough_kernel::FiberUid) {
    LIVE.lock().remove(&fiber);
}

/// The factory this row registers.
pub struct LoopFactory {
    cfg: Arc<LoopConfig>,
    deps: LoopDeps,
}

impl LoopFactory {
    pub fn new(cfg: Arc<LoopConfig>, deps: LoopDeps) -> LoopFactory {
        LoopFactory { cfg, deps }
    }
}

#[async_trait::async_trait]
impl AgentFactory for LoopFactory {
    fn driver(&self) -> &'static str {
        crate::PLUGIN_NAME
    }

    async fn attach(
        &self,
        cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        // `deps.ctx` stays the ROW's context on purpose: the `llm` and `tools` seams register
        // their own innermost hop through the context they are handed and then dispatch from it,
        // so handing them a scoped one would hide the hop from its own dispatch. The agent/*
        // waterfalls are dispatched AT the agent's scope by `wake.rs` (§5's most-specific-wins).
        let deps = self.deps.clone();
        let cfg = self.cfg.clone();
        let cell = Arc::new(cell);
        // `new_cyclic` so the driver can hand a clone of ITSELF to the tasks it spawns; a wake
        // outlives the `&self` that started it, and a raw-pointer registry would be a leak with
        // extra steps.
        let driver = Arc::new_cyclic(|me| LoopDriver {
            cfg,
            deps,
            cell,
            me: me.clone(),
            state: Mutex::new(DriverState::default()),
            drain: DrainGate::new(),
            stopping: AtomicBool::new(false),
            wakes: AtomicUsize::new(0),
        });
        Ok(driver)
    }
}

#[derive(Default)]
struct DriverState {
    running: Option<RunningWake>,
    /// The jot the next wake of ANY kind resumes from (§5).
    resume_from: Option<StepId>,
    /// Every live wake's interrupt token, by wake id: the answer wake replaces `running` before
    /// the wake it interrupted has finished, so the token cannot live only in `running`.
    interrupts: std::collections::BTreeMap<WakeId, CancellationToken>,
    /// Mail waiting for the debounce window to close.
    debouncing: bool,
    cancelled: Option<CancelCause>,
}

struct RunningWake {
    wake: WakeId,
    is_answer: bool,
    /// The wake's kind. The grace step belongs to THIS wake, so it is the kind `model-policy`
    /// decides the grace step's model from.
    kind: WakeKind,
    /// "Started responding": the first reply token has streamed (§5's cutoff).
    streamed: Arc<AtomicBool>,
    /// Messages that joined this wake before it streamed a token (§5).
    joined: Arc<Mutex<Vec<bough_plugin_agents::MessageId>>>,
}

/// One agent's running loop.
pub struct LoopDriver {
    cfg: Arc<LoopConfig>,
    deps: LoopDeps,
    cell: Arc<AgentCell>,
    /// A weak handle to itself, for the tasks a wake spawns.
    me: std::sync::Weak<LoopDriver>,
    state: Mutex<DriverState>,
    drain: DrainGate,
    stopping: AtomicBool,
    /// Wakes in flight; `stop()` drains against it.
    wakes: AtomicUsize,
}

impl LoopDriver {
    /// The fiber this driver's row runs in: the partition key of every process-wide record.
    fn fiber(&self) -> bough_kernel::FiberUid {
        self.deps.ctx.fiber_uid()
    }

    fn mint_wake(&self) -> WakeId {
        WakeId::new(uuid::Uuid::now_v7().to_string())
    }

    /// Start one wake on its own task. The driver returns immediately: §5's latency promise is
    /// that an answer wake starts NOW, concurrently with whatever it interrupted.
    /// Returns the id of the wake it opened, or `None` when the driver is stopping and no wake
    /// was opened. `wake_now` is the caller that needs the answer (§2.5's `WakeRequest`).
    fn spawn_wake(
        self: &Arc<Self>,
        kind: WakeKind,
        trigger: Option<bough_plugin_agents::MessageId>,
    ) -> Option<WakeId> {
        if self.stopping.load(Ordering::SeqCst) {
            return None;
        }
        let wake = self.mint_wake();
        let streamed = Arc::new(AtomicBool::new(false));
        let joined = Arc::new(Mutex::new(Vec::new()));
        let interrupt = CancellationToken::new();
        let resume_from = {
            let mut st = self.state.lock();
            st.running = Some(RunningWake {
                wake: wake.clone(),
                is_answer: kind == WakeKind::Answer,
                kind,
                streamed: streamed.clone(),
                joined: joined.clone(),
            });
            st.interrupts.insert(wake.clone(), interrupt.clone());
            st.resume_from.take()
        };
        let opened = wake.clone();
        let spec = WakeSpec {
            wake,
            kind,
            urgency: match kind {
                WakeKind::Drain => crate::mail::Urgency::Coalesced,
                _ => crate::mail::Urgency::Immediate,
            },
            trigger,
            resume_from,
            interrupt: interrupt.clone(),
            streamed: streamed.clone(),
            joined: joined.clone(),
        };
        let me = self.clone();
        // §5: `status` is the DRIVER-WIDE drain interval, not one wake. Checkpoint-and-answer
        // deliberately runs two wakes at once, so the transitions are edges of the in-flight
        // COUNT: the first wake publishes `Running`, the last one to finish publishes `Idle`.
        // Per-wake transitions made the first finisher publish `Idle` over a wake still open,
        // and `when_idle()` then returned on a half-finished agent.
        let first = self.wakes.fetch_add(1, Ordering::SeqCst) == 0;
        with_live(self.fiber(), |l| {
            l.open.insert(spec.wake.to_string());
        });
        tokio::spawn(async move {
            // The pending-wake flag goes down when the wake actually STARTS, never earlier: it is
            // what stops `when_idle()` returning in the gap between "mail is durable and a wake is
            // armed" and "the wake is running". For the first wake the status edge clears it
            // atomically; a concurrent second wake publishes no edge, so it says so directly.
            if first {
                let _ = me
                    .cell
                    .set_status(bough_plugin_agents::Status::Running)
                    .await;
            } else {
                me.cell.wake_started();
            }
            // §2's ambient initiator, set for the WHOLE wake — the tool pipeline, the journal
            // rows and every waterfall the wake dispatches inline (the `llm/stream` tee among
            // them) run inside this scope, which is the only way a listener can name the agent
            // whose work it is watching.
            let out = bough_plugin_agents::initiator::with(
                me.cell.agent().id().clone(),
                crate::wake::run_wake(&me.cell, spec.clone(), &me.deps),
            )
            .await;
            with_live(me.fiber(), |l| {
                l.open.remove(spec.wake.as_str());
            });
            if spec.kind == WakeKind::Drain {
                me.drain.release();
                with_live(me.fiber(), |l| l.drains = l.drains.saturating_sub(1));
            }
            {
                let mut st = me.state.lock();
                st.interrupts.remove(&spec.wake);
                if st.running.as_ref().map(|r| &r.wake) == Some(&spec.wake) {
                    st.running = None;
                }
            }
            // 17. the standing invariant: unconsumed ordinary mail ⇒ a drain wake IS scheduled.
            me.enforce_standing_invariant().await;
            // A wake never ends leaving WAKE-CLASS mail unclaimed: a message that arrived while
            // this wake was running (§5's "queues as the next wake's first mail") gets its wake
            // here, at the only moment the agent is known to be free.
            me.wake_for_queued_mail();
            if me.wakes.fetch_sub(1, Ordering::SeqCst) == 1 {
                let _ = me.cell.set_status(bough_plugin_agents::Status::Idle).await;
            }
            out
        });
        Some(opened)
    }

    /// §5's standing invariant, checked after every wake and repaired rather than reported: if
    /// ordinary mail is unconsumed, a drain wake is scheduled here and now.
    async fn enforce_standing_invariant(self: &Arc<Self>) {
        let traj = self.cell.agent().traj().clone();
        let Ok(steps) = self
            .deps
            .ledger
            .0
            .steps(&bough_plugin_ledger::StepQuery {
                trajs: vec![traj],
                ..Default::default()
            })
            .await
        else {
            return;
        };
        let consumed = crate::mail::consumed_union(&steps);
        let ordinary = crate::mail::unconsumed(&steps, &consumed)
            .iter()
            .filter(|s| crate::mail::is_ordinary(s))
            .count();
        if ordinary > 0 && !self.drain.in_flight() {
            self.arm_drain();
        }
    }

    /// Whether delivered ordinary mail is still unconsumed. The same fold `enforce_standing_
    /// invariant` runs; a read failure answers "nothing", because a catch-up wake opened on a
    /// failed read would be a wake over nothing.
    async fn has_unconsumed_ordinary_mail(&self) -> bool {
        let traj = self.cell.agent().traj().clone();
        let Ok(steps) = self
            .deps
            .ledger
            .0
            .steps(&bough_plugin_ledger::StepQuery {
                trajs: vec![traj],
                ..Default::default()
            })
            .await
        else {
            return false;
        };
        let consumed = crate::mail::consumed_union(&steps);
        crate::mail::unconsumed(&steps, &consumed)
            .iter()
            .any(crate::mail::is_ordinary)
    }

    /// Start the wake §5 owes any queued wake-class mail once the agent is free again.
    fn wake_for_queued_mail(self: &Arc<Self>) {
        let pending = self.cell.agent().inbox().pending(Target::NextWake);
        let Some(msg) = pending
            .into_iter()
            .find(|m| m.is_andrey() || m.class == bough_plugin_agents::MailClass::Wake)
        else {
            return;
        };
        let kind = if msg.is_andrey() {
            WakeKind::Answer
        } else {
            WakeKind::Catchup
        };
        self.spawn_wake(kind, Some(msg.id.clone()));
    }

    /// Open (or join) the debounce window. Only the first caller owns the slot: §5's one drain
    /// wake in flight per agent.
    fn arm_drain(self: &Arc<Self>) {
        if !self.drain.arm() {
            return;
        }
        with_live(self.fiber(), |l| l.drains += 1);
        {
            let mut st = self.state.lock();
            st.debouncing = true;
        }
        let me = self.clone();
        let delay = Duration::from_millis(self.cfg.drain_debounce_ms);
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            me.state.lock().debouncing = false;
            if me.stopping.load(Ordering::SeqCst) {
                me.drain.release();
                with_live(me.fiber(), |l| l.drains = l.drains.saturating_sub(1));
                return;
            }
            me.spawn_wake(WakeKind::Drain, None);
        });
    }

    /// The preemption of §5, as a DECISION only: what to do with it is `notify`'s, because the
    /// order matters — his answer wake starts BEFORE the interrupted wake is told to stop.
    fn preempt_for(&self, msg: &Message, next: WakeId) -> Option<(Preemption, Option<WakeId>)> {
        let st = self.state.lock();
        let running = st.running.as_ref().map(|r| Running {
            wake: &r.wake,
            is_answer: r.is_answer,
            streamed: r.streamed.load(Ordering::SeqCst),
        });
        let interrupted = st.running.as_ref().map(|r| r.wake.clone());
        crate::preempt::decide(msg, running, next).map(|p| (p, interrupted))
    }

    /// The other half of checkpoint-and-answer: the interrupted wake is told to stop and jots
    /// CONCURRENTLY with the answer wake that has already started. His latency never waits on the
    /// jot (§5).
    fn checkpoint(
        self: &Arc<Self>,
        wake: WakeId,
        by: bough_plugin_agents::MessageId,
        answer: WakeId,
    ) {
        let (interrupt, kind) = {
            let st = self.state.lock();
            (
                st.interrupts.get(&wake).cloned(),
                st.running
                    .as_ref()
                    .filter(|r| r.wake == wake)
                    .map(|r| r.kind)
                    .unwrap_or(WakeKind::Answer),
            )
        };
        let Some(interrupt) = interrupt else { return };
        interrupt.cancel();
        let me = self.clone();
        let agent = self.cell.agent().clone();
        tokio::spawn(async move {
            // The grace jot is a real model step belonging to the interrupted wake, so it runs
            // under the same ambient initiator the wake did (§2).
            let jot =
                bough_plugin_agents::initiator::with(agent.id().clone(), me.jot_for(&wake, kind))
                    .await;
            if let Some(id) = jot {
                me.state.lock().resume_from = Some(id);
            }
            me.deps
                .ctx
                .emit::<bough_plugin_agents::AgentPreempt>(bough_plugin_agents::Preempt {
                    agent: agent.id().clone(),
                    interrupted: wake,
                    by,
                    answer,
                });
        });
    }

    /// P2-D14: a jot ALWAYS exists. The grace step is a real model step; if it fails or times out
    /// the loop writes the synthetic one, built from the wake's last thought steps.
    async fn jot_for(&self, wake: &WakeId, kind: WakeKind) -> Option<StepId> {
        let traj = self.cell.agent().traj().clone();
        let steps = self
            .deps
            .ledger
            .0
            .steps(&bough_plugin_ledger::StepQuery {
                trajs: vec![traj],
                wake: Some(wake.clone()),
                ..Default::default()
            })
            .await
            .unwrap_or_default();
        crate::wake::grace_jot(&self.cell, &self.deps, wake, kind, &steps)
            .await
            .ok()
    }
}

#[async_trait::async_trait]
impl AgentDriver for LoopDriver {
    fn driver(&self) -> &'static str {
        crate::PLUGIN_NAME
    }

    /// §5's catch-up (P3-D16): `Nothing` unless `pending(NextWake)` is non-empty or unconsumed
    /// ordinary mail exists; otherwise ONE wake with the oldest queued item as trigger.
    async fn wake_now(
        &self,
        kind: bough_plugin_agents::WakeKind,
        _cause: bough_plugin_agents::WakeCause,
    ) -> bough_plugin_agents::WakeRequest {
        use bough_plugin_agents::WakeRequest;
        let Some(me) = self.self_arc() else {
            return WakeRequest::Nothing;
        };
        if me.stopping.load(Ordering::SeqCst) {
            return WakeRequest::Nothing;
        }
        // The oldest queued item is the trigger: catch-up is a wake OVER queued mail, and the
        // thing that has waited longest is what it is about.
        let trigger = me
            .cell
            .agent()
            .inbox()
            .pending(Target::NextWake)
            .first()
            .map(|m| m.id.clone());
        if trigger.is_none() && !me.has_unconsumed_ordinary_mail().await {
            // NOTHING AT ALL: no synthetic message, no empty wake, no ledger row. §5's catch-up
            // is over queued mail, and an agent with nothing queued has nothing to catch up on.
            return WakeRequest::Nothing;
        }
        match me.spawn_wake(kind, trigger) {
            Some(wake) => WakeRequest::Started(wake),
            None => WakeRequest::Nothing,
        }
    }

    /// IMMEDIATE for an Andrey message or wake-class mail; a debounced drain otherwise, with one
    /// drain wake in flight per agent (§5).
    async fn notify(&self, receipt: &InboxReceipt, msg: &Message) {
        let me: Arc<LoopDriver> = match self.self_arc() {
            Some(a) => a,
            None => return,
        };
        if me.stopping.load(Ordering::SeqCst) {
            return;
        }
        let next = me.mint_wake();
        // The preemption decision runs FIRST, on both queues. §5's rule is about the SENDER, not
        // about the queue: "an Andrey message ALWAYS gets a fresh sol answer wake, whatever queue
        // it arrived through". Returning early for `next-step` here used to swallow his steer
        // before the rule was ever consulted, and a drain wake would then claim and answer it.
        if let Some((p, interrupted)) = me.preempt_for(msg, next.clone()) {
            match p {
                // JOIN: the running answer wake has not streamed a token yet, so it takes this
                // message at its next STEP boundary and answers both. QUEUE: it has, so the
                // message is left where it is and the next wake takes it as its first mail.
                // Neither opens a second answer wake.
                Preemption::Join { wake } => {
                    let st = me.state.lock();
                    if let Some(r) = st.running.as_ref().filter(|r| r.wake == wake) {
                        r.joined.lock().push(msg.id.clone());
                    }
                    return;
                }
                Preemption::Queue => return,
                Preemption::Checkpoint { answer } => {
                    // ORDER IS THE POINT: his wake opens first, and only then is the interrupted
                    // wake told to stop and jot.
                    me.spawn_wake(WakeKind::Answer, Some(msg.id.clone()));
                    if let Some(wake) = interrupted {
                        me.checkpoint(wake, msg.id.clone(), answer);
                    }
                    return;
                }
            }
        }
        // A non-Andrey steer lands at the next STEP boundary of the running wake: the wake
        // re-reads its inbox there, so there is nothing for the driver to schedule.
        if receipt.target == Target::NextStep && me.state.lock().running.is_some() {
            return;
        }
        match crate::mail::schedule_for(msg, receipt.target, receipt.wake, me.drain.in_flight()) {
            Schedule::Now { kind, trigger } => {
                me.spawn_wake(kind, Some(trigger));
            }
            Schedule::Debounce => me.arm_drain(),
            Schedule::Wait => {}
        }
    }

    async fn cancel(&self, cause: CancelCause, _keep_inbox: bool) {
        {
            let mut st = self.state.lock();
            // First cause wins (§2).
            if st.cancelled.is_none() {
                st.cancelled = Some(cause);
            }
        }
        // `Disposed` never latches a pending wake: it stops the driver instead of arming one.
        if cause == CancelCause::Disposed {
            self.stopping.store(true, Ordering::SeqCst);
        }
        for token in self.state.lock().interrupts.values() {
            token.cancel();
        }
        self.cell.cancel_token().cancel();
    }

    /// Stop and drain: no new wake starts, the in-flight wake ends, returns when idle.
    async fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_millis(self.cfg.status_drain_ms);
        while self.wakes.load(Ordering::SeqCst) > 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if self.wakes.load(Ordering::SeqCst) > 0 {
            // Graceful time is up; the wake is cut short and closes durably as aborted.
            self.cell.cancel_token().cancel();
            for token in self.state.lock().interrupts.values() {
                token.cancel();
            }
            // §2's teardown order is "stop AND DRAIN … returns when idle": returning the instant
            // the token is fired would let `dispose` unwind the agent's scope while a wake is
            // still appending steps at it. The cancel is given the same window again to be
            // OBSERVED, and only a wake that ignores both is left behind — loudly.
            let hard = std::time::Instant::now() + Duration::from_millis(self.cfg.status_drain_ms);
            while self.wakes.load(Ordering::SeqCst) > 0 && std::time::Instant::now() < hard {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            if self.wakes.load(Ordering::SeqCst) > 0 {
                tracing::error!(
                    agent = %self.cell.agent().name(),
                    wakes = self.wakes.load(Ordering::SeqCst),
                    "a wake did not observe its cancellation inside the drain window;                      teardown proceeds and the wake's remaining appends may race the scope"
                );
            }
        }
    }
}

impl LoopDriver {
    fn self_arc(&self) -> Option<Arc<LoopDriver>> {
        self.me.upgrade()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{andrey, ordinary, wake_class};
    use bough_plugin_agents::Target;

    /// V6: an Andrey message gets a fresh ANSWER wake whatever queue it arrived through.
    #[test]
    fn an_andrey_message_gets_a_fresh_answer_wake_from_either_queue() {
        for target in [Target::NextWake, Target::NextStep] {
            let s = crate::mail::schedule_for(&andrey("m", "hi"), target, false, true);
            assert_eq!(
                s,
                Schedule::Now {
                    kind: WakeKind::Answer,
                    trigger: bough_plugin_agents::MessageId::new("m")
                },
                "target {target:?}: his message never waits on a queue or on a drain"
            );
        }
    }

    /// V6: ordinary mail coalesces, and only ONE drain wake is in flight per agent.
    #[test]
    fn only_one_drain_wake_is_in_flight_per_agent() {
        let gate = DrainGate::new();
        assert!(gate.arm(), "the first ordinary message opens the window");
        assert!(
            !gate.arm(),
            "the second joins it rather than opening a second"
        );
        assert_eq!(
            crate::mail::schedule_for(&ordinary("m", None), Target::NextWake, true, true),
            Schedule::Wait,
            "with a drain in flight the mail simply stays unconsumed"
        );
        gate.release();
        assert!(gate.arm(), "the slot is free again once the drain ran");
    }

    #[test]
    fn wake_class_mail_is_immediate_and_an_inject_schedules_nothing() {
        assert!(matches!(
            crate::mail::schedule_for(&wake_class("m", None), Target::NextWake, true, false),
            Schedule::Now {
                kind: WakeKind::Catchup,
                ..
            }
        ));
        assert_eq!(
            crate::mail::schedule_for(&ordinary("m", None), Target::NextStep, false, false),
            Schedule::Wait,
            "an inject waits for something else to wake the agent"
        );
    }
}
