//! §2.4's shortest rule: `Reject` writes `claim/rejected { reason }` and nothing else, EVER.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::AgentsHandle;
use bough_plugin_claims::{
    Actor, ClaimKind, ClaimQuery, ClaimsConfig, ClaimsError, ClaimsHandle, DecideRequest, Decision,
    OpenClaim, ProposeRequest,
};
use bough_plugin_graph_ops::{
    GraphError, GraphHandle, GraphOps, OpOutcome, OpPlan, OpRequest, UndoRequest,
};
use bough_plugin_ledger::{AgentName, LedgerHandle, TrajId};
use bough_plugin_ledger_memory::store::MemoryStore;

/// A graph seam a rejection must never reach.
struct NoGraph;

#[async_trait::async_trait]
impl GraphOps for NoGraph {
    fn provider(&self) -> &'static str {
        "no-graph"
    }
    async fn plan(&self, _req: &OpRequest) -> Result<OpPlan, GraphError> {
        unreachable!("a rejection plans nothing")
    }
    async fn apply(&self, _req: &OpRequest) -> Result<OpOutcome, GraphError> {
        unreachable!("a rejection births nothing")
    }
    async fn undo(&self, _req: &UndoRequest) -> Result<OpOutcome, GraphError> {
        unreachable!("a rejection undoes nothing")
    }
}

struct Fixture {
    ledger: LedgerHandle,
    claims: ClaimsHandle,
}

fn fixture() -> Fixture {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    let agents = AgentsHandle::new(ctx.clone(), ledger.clone());
    let claims = ClaimsHandle::new(
        ctx,
        ledger.clone(),
        agents,
        GraphHandle(Arc::new(NoGraph) as Arc<dyn GraphOps>),
        Arc::new(ClaimsConfig { open_limit: 50 }),
    );
    Fixture { ledger, claims }
}

fn traj() -> TrajId {
    TrajId::new("lane/sol")
}

fn at() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("a fixed instant")
}

async fn propose(f: &Fixture, kind: ClaimKind) -> OpenClaim {
    f.claims
        .propose(ProposeRequest {
            by: AgentName::new("sol"),
            traj: traj(),
            wake: None,
            kind,
            title: "a claim".to_string(),
            body: "the claim, at length".to_string(),
            cites: Vec::new(),
            at: at(),
        })
        .await
        .expect("a proposal is appendable")
}

async fn all(f: &Fixture) -> Vec<bough_plugin_ledger::Step> {
    f.ledger
        .0
        .trajectory_view(&traj())
        .await
        .expect("the view reads")
        .steps
}

#[tokio::test]
async fn a_rejection_records_a_reason() {
    let f = fixture();
    let claim = propose(
        &f,
        ClaimKind::Requirement {
            supersedes: Vec::new(),
        },
    )
    .await;
    let out = f
        .claims
        .decide(DecideRequest {
            claim: claim.claim.clone(),
            decision: Decision::Reject {
                reason: "this is Terra's call, not mine".to_string(),
            },
            actor: Actor::Andrey,
            at: at(),
        })
        .await
        .expect("Andrey may reject");

    let step = f
        .ledger
        .0
        .step(&out.step)
        .await
        .expect("the step reads")
        .expect("the rejection is there");
    assert_eq!(step.kind.as_str(), "claim/rejected");
    assert_eq!(
        step.body.get("reason").and_then(|v| v.as_str()),
        Some("this is Terra's call, not mine"),
        "a rejection without its reason is an unexplained refusal"
    );
    assert_eq!(
        step.body.get("proposal").and_then(|v| v.as_str()),
        Some(claim.proposal.as_str())
    );

    // Deciding twice is refused: a claim is decided once.
    let again = f
        .claims
        .decide(DecideRequest {
            claim: claim.claim,
            decision: Decision::Accept,
            actor: Actor::Andrey,
            at: at(),
        })
        .await
        .expect_err("a decided claim cannot be decided again");
    assert!(matches!(again, ClaimsError::AlreadyDecided(_)), "{again}");
}

#[tokio::test]
async fn a_rejection_births_nothing() {
    let f = fixture();
    // A LANE claim, the one kind whose acceptance would birth a row: rejecting it must reach
    // neither the graph seam (`NoGraph::apply` is `unreachable!`) nor the agents table.
    let claim = propose(
        &f,
        ClaimKind::Lane {
            name: AgentName::new("infra"),
            from_seq: None,
            routing_refs: Default::default(),
            wake_classes: Default::default(),
        },
    )
    .await;
    let before = all(&f).await.len();

    let out = f
        .claims
        .decide(DecideRequest {
            claim: claim.claim,
            decision: Decision::Reject {
                reason: "not yet".to_string(),
            },
            actor: Actor::Andrey,
            at: at(),
        })
        .await
        .expect("a lane claim is rejectable");

    assert_eq!(out.pin, None, "a rejection pins nothing");
    assert_eq!(out.graph, None, "a rejection changes no structure");
    assert_eq!(out.born, None, "a rejection births no lane");
    assert!(
        f.ledger.0.agents().await.expect("rows read").is_empty(),
        "no agents row was written"
    );
    assert_eq!(
        all(&f).await.len(),
        before + 1,
        "a rejection appends exactly one step"
    );
}

#[tokio::test]
async fn a_rejected_claim_leaves_the_open_list() {
    let f = fixture();
    let kept = propose(&f, ClaimKind::Other).await;
    let doomed = propose(&f, ClaimKind::Other).await;
    assert_eq!(
        f.claims
            .open(&ClaimQuery::default())
            .await
            .expect("open reads")
            .len(),
        2
    );

    f.claims
        .decide(DecideRequest {
            claim: doomed.claim.clone(),
            decision: Decision::Reject {
                reason: "no".to_string(),
            },
            actor: Actor::Andrey,
            at: at(),
        })
        .await
        .expect("rejected");

    let open = f
        .claims
        .open(&ClaimQuery::default())
        .await
        .expect("open reads");
    assert_eq!(
        open.iter().map(|c| c.claim.clone()).collect::<Vec<_>>(),
        vec![kept.claim],
        "open is DERIVED: the decided claim is gone, nothing stamped it"
    );
    // The rejected claim is still READABLE, it is simply no longer open.
    assert!(f
        .claims
        .get(&doomed.claim)
        .await
        .expect("get reads")
        .is_some());
    assert!(f.claims.is_decided(&doomed.claim).await.expect("get reads"));
}
