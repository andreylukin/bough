//! The shared fixture for the mail seam's integration tests: a root context, an in-memory ledger,
//! a live agents registry with an inert driver, and a `MailHandle` over both.
//!
//! The driver is deliberately inert. Every claim in these tests is about what the ROUTER wrote to
//! the ledger and to each inbox, and a driver that ran wakes would answer a different question.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::{
    Agent, AgentCell, AgentDriver, AgentError, AgentFactory, AgentsHandle, CancelCause,
    CreateAgent, InboxReceipt, MailClass, Message, Sender, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_ledger::vocabulary::{WakeEnd, WakeEndReason, WakeStart};
use bough_plugin_ledger::{
    AgentName, Append, Cite, Class, LedgerHandle, Order, Ref, Seq, SeqRange, Step, StepQuery,
    StepType, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_mail_router::{Envelope, MailConfig, MailHandle, UnsortedSink};

struct Idle;

#[async_trait::async_trait]
impl AgentDriver for Idle {
    fn driver(&self) -> &'static str {
        "idle"
    }
    async fn wake_now(&self, _k: WakeKind, _c: WakeCause) -> WakeRequest {
        WakeRequest::Nothing
    }
    async fn notify(&self, _r: &InboxReceipt, _m: &Message) {}
    async fn cancel(&self, _c: CancelCause, _keep: bool) {}
    async fn stop(&self) {}
}

struct IdleFactory;

#[async_trait::async_trait]
impl AgentFactory for IdleFactory {
    fn driver(&self) -> &'static str {
        "idle"
    }
    async fn attach(
        &self,
        _cell: AgentCell,
        _mode: bough_plugin_agents::Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        Ok(Arc::new(Idle) as Arc<dyn AgentDriver>)
    }
}

pub struct Fixture {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    pub agents: AgentsHandle,
    pub mail: MailHandle,
}

pub async fn fixture() -> Fixture {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    for def in bough_plugin_mail_router::vocabulary::step_types() {
        ledger.0.register_step_type(def).expect("a fresh map");
    }
    let agents = AgentsHandle::new(ctx.clone(), ledger.clone());
    agents
        .set_factory(&ctx, Arc::new(IdleFactory) as Arc<dyn AgentFactory>)
        .await
        .expect("the slot is free");
    let mail = MailHandle::new(
        ctx.clone(),
        ledger.clone(),
        agents.clone(),
        Arc::new(MailConfig {
            unsorted_traj: "unsorted".into(),
            unsorted_limit: 200,
            deliver_to_dormant: true,
        }),
    );
    Fixture {
        ctx,
        ledger,
        agents,
        mail,
    }
}

impl Fixture {
    /// A live lane routing on `refs`.
    pub async fn lane(&self, name: &str, refs: &[&str]) -> Agent {
        let (agent, disposer) = self
            .agents
            .create(CreateAgent::resident(
                AgentName::new(name),
                TrajId::new(format!("t-{name}")),
                now(),
            ))
            .await
            .expect("the transaction commits");
        // Held for the life of the process: dropping it would tear the agent down under the
        // router, which is not what any of these tests are about.
        std::mem::forget(disposer);
        if !refs.is_empty() {
            self.mail
                .link_ref(&AgentName::new(name), set(refs), now())
                .await
                .expect("a link");
        }
        agent
    }

    /// Every step of one kind, seq-ascending, across every trajectory.
    pub async fn steps(&self, kind: &str) -> Vec<Step> {
        self.ledger
            .0
            .steps(&StepQuery {
                kinds: vec![StepType::new(kind)],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
            .expect("a read")
    }

    /// The same, on one trajectory.
    pub async fn steps_on(&self, traj: &str, kind: &str) -> Vec<Step> {
        self.ledger
            .0
            .steps(&StepQuery {
                trajs: vec![TrajId::new(traj)],
                kinds: vec![StepType::new(kind)],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
            .expect("a read")
    }

    /// Mail delivered to `traj` that no `wake/end.consumed` names.
    pub async fn unconsumed(&self, traj: &str) -> Vec<Step> {
        self.ledger
            .0
            .unconsumed_mail(&TrajId::new(traj))
            .await
            .expect("a read")
    }

    /// Consume one delivered seq on `traj` the way a wake does: a `wake/start` … `wake/end` pair
    /// naming the range. Written here rather than faked, so "consumption is per agent" is tested
    /// against the ledger's own rule.
    pub async fn consume(&self, traj: &str, seq: Seq) {
        let traj = TrajId::new(traj);
        let wake = WakeId::new(uuid_like());
        for (kind, body) in [
            (
                "wake/start",
                serde_json::to_value(WakeStart {
                    urgency: bough_plugin_ledger::vocabulary::Urgency::Coalesced,
                    trigger: None,
                    claimed: vec![],
                })
                .unwrap(),
            ),
            (
                "wake/end",
                serde_json::to_value(WakeEnd {
                    reason: WakeEndReason::Completed,
                    cause: None,
                    consumed: vec![SeqRange { from: seq, to: seq }],
                })
                .unwrap(),
            ),
        ] {
            self.ledger
                .0
                .append(Append {
                    traj: traj.clone(),
                    wake: wake.clone(),
                    kind: StepType::new(kind),
                    class: Class::Thought,
                    body,
                    cites: vec![],
                    at: now(),
                    id: None,
                })
                .await
                .expect("a wake boundary");
        }
    }
}

/// A sink naming one agent.
pub struct NamedSink(pub AgentName);

#[async_trait::async_trait]
impl UnsortedSink for NamedSink {
    fn agent(&self) -> AgentName {
        self.0.clone()
    }
}

pub fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

pub fn set(items: &[&str]) -> BTreeSet<Ref> {
    items.iter().map(Ref::new).collect()
}

pub fn names(items: &[&str]) -> Vec<AgentName> {
    items.iter().map(AgentName::new).collect()
}

pub fn envelope(subject: &str, refs: &[&str]) -> Envelope {
    Envelope {
        from: Sender::Collector("github".into()),
        class: MailClass::Ordinary,
        subject: subject.into(),
        summary: format!("{subject} — summary"),
        text: format!("{subject} — the full body"),
        cites: vec![Cite {
            r#ref: Ref::new("gh:bough/bough#12"),
            url: None,
        }],
        refs: set(refs),
        at: now(),
    }
}

fn uuid_like() -> String {
    format!("wake:{}", uuid::Uuid::now_v7())
}
