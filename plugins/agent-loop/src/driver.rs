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

/// How many drain wakes are scheduled across every live driver. The standing invariant (§5) is
/// about ONE agent, but the invariant runner checks the tree, so the count is the tree's.
static DRAINS_SCHEDULED: AtomicUsize = AtomicUsize::new(0);

/// Whether any live driver has a drain wake scheduled. Read by the standing-invariant check.
pub fn any_drain_scheduled() -> bool {
    DRAINS_SCHEDULED.load(Ordering::SeqCst) > 0
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
    /// "Started responding": the first reply token has streamed (§5's cutoff).
    streamed: Arc<AtomicBool>,
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
    fn mint_wake(&self) -> WakeId {
        WakeId::new(uuid::Uuid::now_v7().to_string())
    }

    /// Start one wake on its own task. The driver returns immediately: §5's latency promise is
    /// that an answer wake starts NOW, concurrently with whatever it interrupted.
    fn spawn_wake(
        self: &Arc<Self>,
        kind: WakeKind,
        trigger: Option<bough_plugin_agents::MessageId>,
    ) {
        if self.stopping.load(Ordering::SeqCst) {
            return;
        }
        let wake = self.mint_wake();
        let streamed = Arc::new(AtomicBool::new(false));
        let interrupt = CancellationToken::new();
        let resume_from = {
            let mut st = self.state.lock();
            st.running = Some(RunningWake {
                wake: wake.clone(),
                is_answer: kind == WakeKind::Answer,
                streamed: streamed.clone(),
            });
            st.interrupts.insert(wake.clone(), interrupt.clone());
            st.resume_from.take()
        };
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
        };
        let me = self.clone();
        self.wakes.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            let _ = me
                .cell
                .set_status(bough_plugin_agents::Status::Running)
                .await;
            let out = crate::wake::run_wake(&me.cell, spec.clone(), &me.deps).await;
            if spec.kind == WakeKind::Drain {
                me.drain.release();
                DRAINS_SCHEDULED.fetch_sub(1, Ordering::SeqCst);
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
            me.wakes.fetch_sub(1, Ordering::SeqCst);
            let _ = me.cell.set_status(bough_plugin_agents::Status::Idle).await;
            out
        });
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
        DRAINS_SCHEDULED.fetch_add(1, Ordering::SeqCst);
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
                DRAINS_SCHEDULED.fetch_sub(1, Ordering::SeqCst);
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
        let interrupt = {
            let st = self.state.lock();
            st.interrupts.get(&wake).cloned()
        };
        let Some(interrupt) = interrupt else { return };
        interrupt.cancel();
        let me = self.clone();
        let agent = self.cell.agent().clone();
        tokio::spawn(async move {
            if let Some(id) = me.jot_for(&wake).await {
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
    async fn jot_for(&self, wake: &WakeId) -> Option<StepId> {
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
        crate::wake::grace_jot(&self.cell, &self.deps, wake, &steps)
            .await
            .ok()
    }
}

#[async_trait::async_trait]
impl AgentDriver for LoopDriver {
    fn driver(&self) -> &'static str {
        crate::PLUGIN_NAME
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
        // A steer lands at the next STEP boundary of the running wake: the wake re-reads its
        // inbox there, so there is nothing for the driver to schedule.
        if receipt.target == Target::NextStep && me.state.lock().running.is_some() {
            return;
        }
        let next = me.mint_wake();
        if let Some((p, interrupted)) = me.preempt_for(msg, next.clone()) {
            match p {
                // Join and Queue both leave the message where it is: the running answer wake
                // claims it at its next step boundary, or the next wake takes it as its first
                // mail. Nothing else starts a second answer wake.
                Preemption::Join { .. } | Preemption::Queue => return,
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
        match crate::mail::schedule_for(msg, receipt.target, receipt.wake, me.drain.in_flight()) {
            Schedule::Now { kind, trigger } => me.spawn_wake(kind, Some(trigger)),
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
