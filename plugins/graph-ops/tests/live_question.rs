//! V3, end to end over the LIVE mail seam (no `LeaderAsk` stand-in): a contested routing ref on a
//! split refuses and reaches Andrey as a real `leader/question` step plus real wake-class mail to
//! the leader's inbox; and an envelope matching zero routing refs lands in the same unsorted
//! queue. Both halves are asserted against what the ledger actually holds.

use crate::common;

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::{
    Agent, AgentCell, AgentDriver, AgentError, AgentFactory, AgentsHandle, CancelCause,
    CreateAgent, InboxReceipt, MailClass, Message, Sender, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_graph_ops::{
    ChildSpec, GraphConfig, GraphError, GraphInner, GraphOps, LeaderAsk, MailAsk, OpRequest,
    SplitRequest,
};
use bough_plugin_ledger::{
    AgentName, Cite, LedgerHandle, Order, Ref, Step, StepQuery, StepType, TrajId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_mail_router::{Envelope, MailConfig, MailHandle, UnsortedSink, ASK_CLASS_REF};
use bough_plugin_rollups::{RollupsHandle, Summarizer};
use common::{base, refs, RecordingDigests};
use parking_lot::Mutex;

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

struct NamedSink(AgentName);

impl UnsortedSink for NamedSink {
    fn agent(&self) -> AgentName {
        self.0.clone()
    }
}

struct Live {
    ledger: LedgerHandle,
    mail: MailHandle,
    agents: AgentsHandle,
    graph: GraphInner,
    ctx: Context,
}

async fn live() -> Live {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    for def in bough_plugin_mail_router::vocabulary::step_types() {
        ledger.0.register_step_type(def).expect("a fresh map");
    }
    for def in bough_plugin_graph_ops::step_types() {
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
            tolerate_absent_lane: true,
        }),
    );
    let digests = Arc::new(RecordingDigests {
        ledger: ledger.clone(),
        calls: Mutex::new(Vec::new()),
    });
    let graph = GraphInner {
        ctx: ctx.clone(),
        ledger: ledger.clone(),
        rollups: RollupsHandle(digests as Arc<dyn Summarizer>),
        // THE LIVE SEAM: the real `ctx.mail.ask_leader`, not a recorder.
        ask: Arc::new(MailAsk(mail.clone())) as Arc<dyn LeaderAsk>,
        cfg: Arc::new(GraphConfig {
            digest_on_fork: false,
        }),
    };
    Live {
        ledger,
        mail,
        agents,
        graph,
        ctx,
    }
}

impl Live {
    async fn lane(&self, name: &str, routing: &[&str]) -> Agent {
        let (agent, disposer) = self
            .agents
            .create(CreateAgent::resident(
                AgentName::new(name),
                TrajId::new(format!("lane/{name}")),
                base(),
            ))
            .await
            .expect("the transaction commits");
        std::mem::forget(disposer);
        if !routing.is_empty() {
            self.mail
                .link_ref(&AgentName::new(name), set(routing), base())
                .await
                .expect("a link");
        }
        agent
    }

    async fn steps_on(&self, traj: &str, kind: &str) -> Vec<Step> {
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
}

fn set(items: &[&str]) -> BTreeSet<Ref> {
    items.iter().map(Ref::new).collect()
}

fn contested() -> OpRequest {
    let child = |name: &str| ChildSpec {
        agent: Some(AgentName::new(name)),
        traj: TrajId::new(format!("lane/{name}")),
        routing_refs: refs(&["gh:o/r"]),
        wake_classes: Default::default(),
    };
    OpRequest::Split(SplitRequest {
        parent: AgentName::new("sol"),
        at_seq: None,
        children: vec![child("a"), child("b")],
        reason: "two concerns".into(),
        by: bough_plugin_rollups::Attribution::Andrey,
        cites: vec![],
        at: base(),
    })
}

/// The conflict half of V3, over the real router.
#[tokio::test]
async fn a_routing_conflict_reaches_andrey_as_a_real_leader_question_step() {
    let f = live().await;
    f.lane("sol", &["gh:o/r"]).await;
    f.lane("leader", &[ASK_CLASS_REF]).await;

    let err = f
        .graph
        .apply(&contested())
        .await
        .expect_err("a contested ref is never guessed");
    assert!(matches!(err, GraphError::Ambiguous { .. }), "{err}");

    // A real step, on the unsorted trajectory, naming the contested ref and both claimants.
    let qs = f.steps_on("unsorted", "leader/question").await;
    assert_eq!(qs.len(), 1, "one question, once");
    let about = qs[0]
        .body
        .get("about")
        .and_then(|v| v.as_str())
        .expect("the question text");
    assert!(about.contains("gh:o/r"), "{about}");
    assert!(about.contains('a') && about.contains('b'), "{about}");
    assert_eq!(
        qs[0].body.get("asked_by").and_then(|v| v.as_str()),
        Some("graph-ops")
    );

    // And it reached the leader's inbox as wake-class mail carrying `class:ask`.
    let delivered = f.steps_on("lane/leader", "mail/delivered").await;
    assert_eq!(delivered.len(), 1);
    assert_eq!(
        delivered[0].body.get("class").and_then(|c| c.as_str()),
        Some("wake")
    );

    // NOTHING was guessed: no split, no child rows, no child trajectories.
    assert!(f.steps_on("lane/sol", "graph/split").await.is_empty());
    assert!(f
        .ledger
        .0
        .agent(&AgentName::new("a"))
        .await
        .unwrap()
        .is_none());
    assert!(f
        .ledger
        .0
        .agent(&AgentName::new("b"))
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        f.ledger
            .0
            .agent(&AgentName::new("sol"))
            .await
            .unwrap()
            .expect("sol still stands")
            .routing_refs,
        set(&["gh:o/r"]),
        "the parent kept every ref it held"
    );
}

/// The zero-match half of V3, over the same live wiring.
#[tokio::test]
async fn mail_matching_no_routing_ref_lands_in_the_leaders_unsorted_queue() {
    let f = live().await;
    f.lane("docs", &["repo:wiki"]).await;
    f.lane("leader", &[]).await;
    let _sink = f
        .mail
        .unsorted_sink(
            &f.ctx,
            Arc::new(NamedSink(AgentName::new("leader"))) as Arc<dyn UnsortedSink>,
        )
        .await
        .expect("the sink mounts");

    let report = f
        .mail
        .route(Envelope {
            from: Sender::Collector("github".into()),
            class: MailClass::Ordinary,
            subject: "CI is red".into(),
            summary: "CI is red".into(),
            text: "the full body".into(),
            cites: vec![Cite {
                r#ref: Ref::new("gh:bough/bough#12"),
                url: None,
            }],
            refs: set(&["repo:bough"]),
            at: base(),
        })
        .await
        .expect("a route");

    assert!(report.matched.is_empty(), "nobody routes on `repo:bough`");
    assert_eq!(f.mail.unsorted(50).await.expect("a read").len(), 1);
    // It went to the leader, and to nobody else — `docs` was not guessed at.
    assert_eq!(f.steps_on("lane/leader", "mail/delivered").await.len(), 1);
    assert!(f.steps_on("lane/docs", "mail/delivered").await.is_empty());
}
