//! §2.4 over a REAL store: an acceptance is the only thing that makes a requirement a pin, an
//! edit pins the EDITED text without rewriting the proposal, and a wake may not accept.

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::{AgentId, AgentsHandle};
use bough_plugin_claims::{
    Actor, ClaimKind, ClaimQuery, ClaimsConfig, ClaimsError, ClaimsHandle, DecideRequest, Decision,
    OpenClaim, ProposeRequest,
};
use bough_plugin_graph_ops::{
    GraphError, GraphHandle, GraphOps, OpOutcome, OpPlan, OpRequest, UndoRequest,
};
use bough_plugin_ledger::{
    AgentName, LedgerHandle, Order, StepQuery, StepType, TrajId, TrajectoryView,
};
use bough_plugin_ledger_memory::store::MemoryStore;

/// A graph seam that is never reached: nothing in this file accepts a structural claim, and a
/// call to it would be the bug.
struct NoGraph;

#[async_trait::async_trait]
impl GraphOps for NoGraph {
    fn provider(&self) -> &'static str {
        "no-graph"
    }
    async fn plan(&self, _req: &OpRequest) -> Result<OpPlan, GraphError> {
        unreachable!("no test in this file plans a graph op")
    }
    async fn apply(&self, _req: &OpRequest) -> Result<OpOutcome, GraphError> {
        unreachable!("accepting a requirement must never reach the graph seam")
    }
    async fn undo(&self, _req: &UndoRequest) -> Result<OpOutcome, GraphError> {
        unreachable!("no test in this file undoes a graph op")
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
    let graph = GraphHandle(Arc::new(NoGraph) as Arc<dyn GraphOps>);
    let claims = ClaimsHandle::new(
        ctx,
        ledger.clone(),
        agents,
        graph,
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

async fn requirement(
    f: &Fixture,
    title: &str,
    supersedes: Vec<bough_plugin_ledger::StepId>,
) -> OpenClaim {
    f.claims
        .propose(ProposeRequest {
            by: AgentName::new("sol"),
            traj: traj(),
            wake: None,
            kind: ClaimKind::Requirement { supersedes },
            title: title.to_string(),
            body: format!("{title}, at length"),
            cites: Vec::new(),
            at: at(),
        })
        .await
        .expect("a proposal is appendable")
}

async fn steps_of(f: &Fixture, kind: &str) -> Vec<bough_plugin_ledger::Step> {
    f.ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj()],
            kinds: vec![StepType::new(kind)],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the store reads")
}

#[tokio::test]
async fn accepting_a_requirement_appends_a_pin() {
    let f = fixture();
    let claim = requirement(&f, "every step carries a wake id", Vec::new()).await;
    assert_eq!(
        f.claims
            .open(&ClaimQuery::default())
            .await
            .expect("open reads")
            .len(),
        1,
        "a proposal with no decision is open"
    );

    let out = f
        .claims
        .decide(DecideRequest {
            claim: claim.claim.clone(),
            decision: Decision::Accept,
            actor: Actor::Andrey,
            at: at(),
        })
        .await
        .expect("Andrey may accept");

    // The acceptance, and the pin it produced.
    let accepted = steps_of(&f, "claim/accepted").await;
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0].id, out.step);
    assert_eq!(
        accepted[0].body.get("edited"),
        Some(&serde_json::json!(false))
    );
    assert_eq!(
        accepted[0].class,
        bough_plugin_ledger::Class::Evidence,
        "an acceptance is a truth claim and cites the proposal"
    );

    let pin = out.pin.expect("an accepted requirement IS a pin (§3)");
    let pins = steps_of(&f, "pin/set").await;
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].id, pin);
    assert_eq!(
        pins[0].body.get("text").and_then(|v| v.as_str()),
        Some(claim.body.as_str()),
        "the pin carries the claim's own text"
    );
    // And the projection sees it as live.
    let live = f.ledger.0.live_pins(&[traj()]).await.expect("pins read");
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].step, pin);

    // Nothing structural happened.
    assert_eq!(out.graph, None);
    assert_eq!(out.born, None);
    // And the claim has left the open list.
    assert!(f
        .claims
        .open(&ClaimQuery::default())
        .await
        .expect("open reads")
        .is_empty());
}

#[tokio::test]
async fn the_pin_supersedes_the_requirements_previous_pin() {
    let f = fixture();
    let first = requirement(&f, "pins ride every projection", Vec::new()).await;
    let first_pin = f
        .claims
        .decide(DecideRequest {
            claim: first.claim.clone(),
            decision: Decision::Accept,
            actor: Actor::Andrey,
            at: at(),
        })
        .await
        .expect("accepted")
        .pin
        .expect("a pin");

    // The SAME requirement, restated and accepted again: §3's relief valve is a new pin that
    // supersedes the old one, never an edit of the old row.
    let again = requirement(
        &f,
        "pins ride every projection, verbatim",
        vec![first_pin.clone()],
    )
    .await;
    let second_pin = f
        .claims
        .decide(DecideRequest {
            claim: again.claim.clone(),
            decision: Decision::Accept,
            actor: Actor::Andrey,
            at: at(),
        })
        .await
        .expect("accepted")
        .pin
        .expect("a pin");

    let pins = steps_of(&f, "pin/set").await;
    assert_eq!(pins.len(), 2, "the old pin row is untouched");
    let supersedes = pins[1]
        .body
        .get("supersedes")
        .and_then(|v| v.as_array())
        .expect("a supersedes list");
    assert!(
        supersedes.contains(&serde_json::json!(first_pin.as_str())),
        "{supersedes:?}"
    );

    // Only the newer pin is live.
    let live = f.ledger.0.live_pins(&[traj()]).await.expect("pins read");
    assert_eq!(live.len(), 1, "{live:?}");
    assert_eq!(live[0].step, second_pin);
}

#[tokio::test]
async fn an_edit_accepts_with_edited_true_and_pins_the_edited_text() {
    let f = fixture();
    let claim = requirement(&f, "the sloppy wording", Vec::new()).await;
    let out = f
        .claims
        .decide(DecideRequest {
            claim: claim.claim.clone(),
            decision: Decision::Edit {
                title: "the tightened wording".to_string(),
                body: "what Andrey actually meant".to_string(),
            },
            actor: Actor::Andrey,
            at: at(),
        })
        .await
        .expect("an edit is an acceptance");

    let accepted = steps_of(&f, "claim/accepted").await;
    assert_eq!(
        accepted[0].body.get("edited"),
        Some(&serde_json::json!(true)),
        "an edit is an acceptance flagged as one"
    );
    let pins = steps_of(&f, "pin/set").await;
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].id, out.pin.expect("a pin"));
    assert_eq!(
        pins[0].body.get("title").and_then(|v| v.as_str()),
        Some("the tightened wording")
    );
    assert_eq!(
        pins[0].body.get("text").and_then(|v| v.as_str()),
        Some("what Andrey actually meant"),
        "the EDITED text is what gets pinned"
    );
}

#[tokio::test]
async fn the_proposal_step_is_never_rewritten() {
    let f = fixture();
    let claim = requirement(&f, "the original words", Vec::new()).await;
    let before: TrajectoryView = f
        .ledger
        .0
        .trajectory_view(&traj())
        .await
        .expect("the view reads");
    let proposal_before = f
        .ledger
        .0
        .step(&claim.proposal)
        .await
        .expect("the step reads")
        .expect("the proposal is there");

    f.claims
        .decide(DecideRequest {
            claim: claim.claim.clone(),
            decision: Decision::Edit {
                title: "different words".to_string(),
                body: "a different body entirely".to_string(),
            },
            actor: Actor::Andrey,
            at: at(),
        })
        .await
        .expect("an edit");

    let proposal_after = f
        .ledger
        .0
        .step(&claim.proposal)
        .await
        .expect("the step reads")
        .expect("the proposal is still there");
    assert_eq!(
        proposal_before, proposal_after,
        "the edit is a NEW fact citing the proposal, never a rewrite of it"
    );
    let after = f
        .ledger
        .0
        .trajectory_view(&traj())
        .await
        .expect("the view reads");
    assert_eq!(
        after.steps.len(),
        before.steps.len() + 2,
        "an edit appends exactly the acceptance and the pin"
    );
}

#[tokio::test]
async fn an_accept_from_a_wake_is_refused() {
    let f = fixture();
    let claim = requirement(&f, "a claim the agent would love to accept", Vec::new()).await;

    // INSIDE a wake: the ambient initiator is set, which is exactly the condition §16 refuses.
    let err = bough_plugin_agents::initiator::with(AgentId::new("sol"), async {
        f.claims
            .decide(DecideRequest {
                claim: claim.claim.clone(),
                decision: Decision::Accept,
                actor: Actor::Andrey,
                at: at(),
            })
            .await
            .expect_err("a wake may not accept its own claim")
    })
    .await;
    assert!(matches!(err, ClaimsError::NotAndreysAct { .. }), "{err}");
    assert!(err.to_string().contains("Andrey's act"), "{err}");

    // NOTHING was written, and the claim is still open.
    assert!(steps_of(&f, "claim/accepted").await.is_empty());
    assert!(steps_of(&f, "pin/set").await.is_empty());
    assert_eq!(
        f.claims
            .open(&ClaimQuery::default())
            .await
            .expect("open reads")
            .len(),
        1
    );

    // Outside the wake the same call lands: the refusal is the ambient scope, not the claim.
    f.claims
        .decide(DecideRequest {
            claim: claim.claim,
            decision: Decision::Accept,
            actor: Actor::Andrey,
            at: at(),
        })
        .await
        .expect("Andrey, outside any wake, may accept");
    let _ = BTreeSet::<u8>::new();
}
