//! §2.4's structural half: a claim accepted as a NEW LANE births an `agents` row and a live
//! resident through `ctx.graph`, or neither. The graph seam here is a recording double: what this
//! file pins is what CLAIMS asks of it and what it does with the answer, not how a split is
//! written (that is `graph-ops`' own suite).

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::{
    AgentCell, AgentDriver, AgentError, AgentFactory, AgentsHandle, Attach, CancelCause,
    InboxReceipt, Message, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_claims::{
    Actor, ClaimKind, ClaimsConfig, ClaimsHandle, DecideRequest, Decision, OpenClaim,
    ProposeRequest,
};
use bough_plugin_graph_ops::{
    GraphError, GraphHandle, GraphOps, OpKind, OpOutcome, OpPlan, OpRequest, UndoRequest,
};
use bough_plugin_ledger::{
    AgentName, AgentRow, LedgerHandle, Ref, Seq, StepQuery, StepType, TrajId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use parking_lot::Mutex;

/// A graph seam that records the request and writes the row a real bud would write.
struct RecordingGraph {
    ledger: LedgerHandle,
    seen: Mutex<Vec<OpRequest>>,
    undone: Mutex<Vec<bough_plugin_ledger::StepId>>,
    refuse: bool,
}

#[async_trait::async_trait]
impl GraphOps for RecordingGraph {
    fn provider(&self) -> &'static str {
        "recording-graph"
    }
    async fn plan(&self, _req: &OpRequest) -> Result<OpPlan, GraphError> {
        unreachable!("claims applies, it does not plan")
    }
    async fn apply(&self, req: &OpRequest) -> Result<OpOutcome, GraphError> {
        self.seen.lock().push(req.clone());
        if self.refuse {
            return Err(GraphError::Ambiguous {
                detail: "two lanes already carry `repo:bough`".to_string(),
            });
        }
        let bud = match req {
            OpRequest::Bud(b) => b,
            other => panic!("a lane claim must ask for a BUD, got {other:?}"),
        };
        let child = &bud.child;
        let name = child.agent.clone().expect("a lane bud names its agent");
        self.ledger
            .0
            .put_agent(AgentRow {
                name: name.clone(),
                traj: child.traj.clone(),
                routing_refs: child.routing_refs.clone(),
                wake_classes: child.wake_classes.clone(),
                model_override: None,
                tick_floor: None,
                digest_rollup: None,
            })
            .await?;
        Ok(OpOutcome {
            kind: OpKind::Bud,
            step: bough_plugin_ledger::StepId::new("graph-bud-step"),
            trajs: vec![child.traj.clone()],
            edges: 1,
            digests: Vec::new(),
            rows_written: vec![name],
            rows_deleted: Vec::new(),
            undo_shape: None,
        })
    }
    /// The real `graph-ops` undo of a bud deletes the child's row. `claims` calls it when the
    /// lane cannot be brought up: the bud is already COMMITTED by then, so deleting the row alone
    /// would leave a cited `graph/bud` naming an agent that does not exist (P5-D8).
    async fn undo(&self, req: &UndoRequest) -> Result<OpOutcome, GraphError> {
        self.undone.lock().push(req.of.clone());
        let mut deleted = Vec::new();
        for row in self.ledger.0.agents().await? {
            if row.traj.as_str().contains("infra") || row.name.as_str() == "infra" {
                self.ledger.0.delete_agent(&row.name).await?;
                deleted.push(row.name);
            }
        }
        Ok(OpOutcome {
            kind: OpKind::Undo,
            step: bough_plugin_ledger::StepId::new("graph-undo-step"),
            trajs: Vec::new(),
            edges: 0,
            digests: Vec::new(),
            rows_written: Vec::new(),
            rows_deleted: deleted,
            undo_shape: Some(bough_plugin_graph_ops::UndoShape::Pointers),
        })
    }
}

/// The smallest factory that can attach: a resume needs one, and a lane with no resident is not
/// born (§2.4).
#[derive(Default)]
struct StubFactory {
    refuse: bool,
    attached: Mutex<Vec<AgentName>>,
}

#[async_trait::async_trait]
impl AgentFactory for StubFactory {
    fn driver(&self) -> &'static str {
        "stub-loop"
    }
    async fn attach(
        &self,
        cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        if self.refuse {
            return Err(AgentError::NoFactory);
        }
        self.attached.lock().push(cell.agent().name().clone());
        Ok(Arc::new(StubDriver) as Arc<dyn AgentDriver>)
    }
}

struct StubDriver;

#[async_trait::async_trait]
impl AgentDriver for StubDriver {
    fn driver(&self) -> &'static str {
        "stub-loop"
    }
    async fn notify(&self, _receipt: &InboxReceipt, _msg: &Message) {}
    async fn cancel(&self, _cause: CancelCause, _keep_inbox: bool) {}
    async fn stop(&self) {}
    async fn wake_now(&self, _kind: WakeKind, _cause: WakeCause) -> WakeRequest {
        WakeRequest::Nothing
    }
}

struct Fixture {
    ledger: LedgerHandle,
    agents: AgentsHandle,
    claims: ClaimsHandle,
    graph: Arc<RecordingGraph>,
    factory: Arc<StubFactory>,
}

async fn fixture(graph_refuses: bool, factory_refuses: bool) -> Fixture {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    let agents = AgentsHandle::new(ctx.clone(), ledger.clone());
    let factory = Arc::new(StubFactory {
        refuse: factory_refuses,
        attached: Mutex::new(Vec::new()),
    });
    agents
        .set_factory(&ctx, factory.clone() as Arc<dyn AgentFactory>)
        .await
        .expect("the slot is free");
    let graph = Arc::new(RecordingGraph {
        ledger: ledger.clone(),
        seen: Mutex::new(Vec::new()),
        undone: Mutex::new(Vec::new()),
        refuse: graph_refuses,
    });
    let claims = ClaimsHandle::new(
        ctx,
        ledger.clone(),
        agents.clone(),
        GraphHandle(graph.clone() as Arc<dyn GraphOps>),
        Arc::new(ClaimsConfig { open_limit: 50 }),
    );
    Fixture {
        ledger,
        agents,
        claims,
        graph,
        factory,
    }
}

fn parent() -> AgentName {
    AgentName::new("sol")
}

fn traj() -> TrajId {
    TrajId::new("lane/sol")
}

fn at() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("a fixed instant")
}

fn routing() -> BTreeSet<Ref> {
    BTreeSet::from([Ref::new("repo:bough"), Ref::new("class:ask")])
}

async fn lane_claim(f: &Fixture) -> OpenClaim {
    f.claims
        .propose(ProposeRequest {
            by: parent(),
            traj: traj(),
            wake: None,
            kind: ClaimKind::Lane {
                name: AgentName::new("infra"),
                from_seq: Some(Seq(1)),
                routing_refs: routing(),
                wake_classes: BTreeSet::from(["ask".to_string()]),
            },
            title: "infra deserves its own lane".to_string(),
            body: "the deploy work has its own rhythm".to_string(),
            cites: Vec::new(),
            at: at(),
        })
        .await
        .expect("a proposal is appendable")
}

async fn accept(
    f: &Fixture,
    claim: &OpenClaim,
) -> Result<bough_plugin_claims::DecideOutcome, bough_plugin_claims::ClaimsError> {
    f.claims
        .decide(DecideRequest {
            claim: claim.claim.clone(),
            decision: Decision::Accept,
            actor: Actor::Andrey,
            at: at(),
        })
        .await
}

#[tokio::test]
async fn accepting_a_lane_claim_births_an_agents_row() {
    let f = fixture(false, false).await;
    let claim = lane_claim(&f).await;
    let out = accept(&f, &claim).await.expect("Andrey may accept a lane");

    assert_eq!(out.born, Some(AgentName::new("infra")));
    let row = f
        .ledger
        .0
        .agent(&AgentName::new("infra"))
        .await
        .expect("rows read")
        .expect("the row exists");
    assert_eq!(row.traj, TrajId::new("lane/infra"));
    // A ROW and a LIVE agent, not one of the two.
    assert_eq!(*f.factory.attached.lock(), vec![AgentName::new("infra")]);
    assert!(
        f.agents.by_name(&AgentName::new("infra")).is_some(),
        "the resident is live"
    );
    // And the acceptance is on the ledger.
    let accepted = f
        .ledger
        .0
        .steps(&StepQuery {
            kinds: vec![StepType::new("claim/accepted")],
            ..Default::default()
        })
        .await
        .expect("steps read");
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0].id, out.step);
}

#[tokio::test]
async fn the_new_lane_is_a_bud_of_the_proposing_trajectory() {
    let f = fixture(false, false).await;
    let claim = lane_claim(&f).await;
    accept(&f, &claim).await.expect("accepted");

    let seen = f.graph.seen.lock().clone();
    assert_eq!(seen.len(), 1, "exactly one graph op per acceptance");
    match &seen[0] {
        OpRequest::Bud(b) => {
            assert_eq!(b.parent, parent(), "the bud is of the PROPOSING lane (§4)");
            assert_eq!(b.at_seq, Seq(1), "at the point the claim named");
            assert_eq!(b.child.agent, Some(AgentName::new("infra")));
            assert!(
                b.cites
                    .iter()
                    .any(|c| c.r#ref.as_str().contains(claim.proposal.as_str())),
                "a structure change cites what justified it: {:?}",
                b.cites
            );
        }
        other => panic!("a lane claim is a BUD, never a split: {other:?}"),
    }
}

#[tokio::test]
async fn the_new_lane_carries_the_routing_refs_from_the_claim() {
    let f = fixture(false, false).await;
    let claim = lane_claim(&f).await;
    accept(&f, &claim).await.expect("accepted");

    let row = f
        .ledger
        .0
        .agent(&AgentName::new("infra"))
        .await
        .expect("rows read")
        .expect("the row exists");
    assert_eq!(
        row.routing_refs,
        routing(),
        "a born lane routes on the refs the claim named, not on none"
    );
    assert_eq!(row.wake_classes, BTreeSet::from(["ask".to_string()]));
    // And the parent kept its own: a bud never moves the parent's routing (§4).
    assert!(f
        .ledger
        .0
        .agent(&parent())
        .await
        .expect("rows read")
        .is_none());
}

#[tokio::test]
async fn a_failed_birth_leaves_no_row_and_no_acceptance() {
    // The graph seam refuses: routing is ambiguous, which §4 makes a question rather than a guess.
    let f = fixture(true, false).await;
    let claim = lane_claim(&f).await;
    let err = accept(&f, &claim)
        .await
        .expect_err("a refused op is a refused acceptance");
    assert!(err.to_string().contains("ambiguous"), "{err}");

    assert!(
        f.ledger.0.agents().await.expect("rows read").is_empty(),
        "no row"
    );
    assert!(
        f.ledger
            .0
            .steps(&StepQuery {
                kinds: vec![StepType::new("claim/accepted")],
                ..Default::default()
            })
            .await
            .expect("steps read")
            .is_empty(),
        "and no acceptance: a lane claim births a row and a resident, or neither"
    );
    assert!(f.agents.by_name(&AgentName::new("infra")).is_none());
    // The graph refused, so no bud committed and there is nothing to undo.
    assert!(f.graph.undone.lock().is_empty(), "nothing to undo");
    // The claim is still open, so Andrey can decide it again once the ambiguity is settled.
    assert_eq!(
        f.claims
            .open(&Default::default())
            .await
            .expect("open reads")
            .len(),
        1
    );

    // The OTHER half of the transaction: the row is written but the resident cannot attach.
    let g = fixture(false, true).await;
    let c2 = lane_claim(&g).await;
    let err = accept(&g, &c2).await.expect_err("a lane with no resident");
    assert!(err.to_string().contains("could not be brought up"), "{err}");
    assert!(
        g.ledger.0.agents().await.expect("rows read").is_empty(),
        "the row the graph wrote is rolled back with the failed birth"
    );
    // And the rollback is a real one: the bud had already COMMITTED — child trajectory, edge,
    // digest and the cited `graph/bud` step — so deleting the row alone would leave that cited
    // fact naming an agent that does not exist, which is exactly what P5-D8's append-last
    // ordering exists to prevent. The op is UNDONE through the operation that exists for it.
    assert_eq!(
        g.graph.undone.lock().len(),
        1,
        "the committed bud is undone, not merely orphaned"
    );
    assert!(g
        .ledger
        .0
        .steps(&StepQuery {
            kinds: vec![StepType::new("claim/accepted")],
            ..Default::default()
        })
        .await
        .expect("steps read")
        .is_empty());
}
