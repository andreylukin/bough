//! §4's bud: a split at a PAST point, and the parent never pauses. A bud with no `agents` row is
//! a FORK — a trajectory and an edge, no row and no routing — and promoting it is adding the row
//! and nothing else.

mod common;

use bough_plugin_graph_ops::{
    BudRequest, ChildSpec, GraphBud, GraphOps, OpKind, OpRequest, GRAPH_BUD,
};
use bough_plugin_ledger::{AgentName, AgentRow, Class, EdgeKind, Seq};
use bough_plugin_rollups::Attribution;
use common::{base, fx, refs, traj};

fn bud(child: ChildSpec, at: Seq) -> OpRequest {
    OpRequest::Bud(BudRequest {
        parent: AgentName::new("sol"),
        at_seq: at,
        child,
        reason: "a question from three wakes ago".into(),
        by: Attribution::Andrey,
        cites: vec![],
        at: base(),
    })
}

fn lane_child(name: &str, rs: &[&str]) -> ChildSpec {
    ChildSpec {
        agent: Some(AgentName::new(name)),
        traj: traj(name),
        routing_refs: refs(rs),
        wake_classes: ["ask".to_string()].into_iter().collect(),
    }
}

fn headless(name: &str) -> ChildSpec {
    ChildSpec {
        agent: None,
        traj: traj(name),
        routing_refs: Default::default(),
        wake_classes: Default::default(),
    }
}

#[tokio::test]
async fn a_bud_from_a_past_seq_leaves_the_parent_chain_whole() {
    let f = fx();
    let parent = f.lane("sol", &["gh:o/r"]).await;
    let before = f.steps(&parent.traj).await;

    let out = f
        .graph
        .apply(&bud(lane_child("scout", &[]), Seq(3)))
        .await
        .expect("a past point is taken as given");
    assert_eq!(out.kind, OpKind::Bud);

    let after = f.steps(&parent.traj).await;
    // The parent gained the cited step and NOTHING else moved: same ids, same seqs, in order.
    assert_eq!(after.len(), before.len() + 1);
    for (a, b) in after.iter().zip(before.iter()) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.seq, b.seq);
    }
    let edge = &f.ledger.0.edges(&traj("scout")).await.expect("readable")[0];
    assert_eq!(edge.kind, EdgeKind::Ancestor);
    assert_eq!(edge.at_seq, Seq(3), "the PAST point, not the head");
}

#[tokio::test]
async fn the_parents_running_wake_completes_untouched() {
    let f = fx();
    let parent = f.lane("sol", &["gh:o/r"]).await;
    // A wake is OPEN on the parent right now. §4: the parent never pauses.
    let w = f.open_wake(&parent.traj, "sol/live").await;
    f.append(
        &parent.traj,
        &w,
        "pin/set",
        Class::Thought,
        serde_json::json!({ "title": "mid-wake", "text": "still working" }),
        vec![],
    )
    .await;

    f.graph
        .apply(&bud(lane_child("scout", &[]), Seq(6)))
        .await
        .expect("a bud below the open wake is legal while it runs");

    // The wake then ENDS normally, on the same chain, at the seq after everything the bud wrote.
    let end = f
        .append(
            &parent.traj,
            &w,
            "wake/end",
            Class::Thought,
            serde_json::json!({ "reason": "completed", "consumed": [] }),
            vec![],
        )
        .await;
    let chain = f.steps(&parent.traj).await;
    assert_eq!(chain.last().unwrap().id, end.id);
    assert!(
        chain.iter().any(|s| s.kind.as_str() == GRAPH_BUD),
        "the bud step is on the chain, between the wake's own steps"
    );
    // An EXPLICIT point INSIDE the open wake is an error, never a silent adjustment (P5-D7).
    let err = f
        .graph
        .apply(&bud(lane_child("other", &[]), Seq(8)))
        .await
        .expect_err("a point inside an open wake is refused");
    assert!(
        matches!(err, bough_plugin_graph_ops::GraphError::OpenWake { .. }),
        "{err}"
    );
}

#[tokio::test]
async fn the_child_digest_names_src_trajs() {
    let f = fx();
    f.lane("sol", &["gh:o/r"]).await;
    let out = f
        .graph
        .apply(&bud(lane_child("scout", &[]), Seq(3)))
        .await
        .expect("applies");
    assert_eq!(out.digests.len(), 1);
    let calls = f.digests.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].parents, vec![traj("sol")]);
    assert!(!calls[0].reconcile);
    let rows = f.rollups_on(&traj("scout")).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].src_trajs, vec![traj("sol")]);
}

#[tokio::test]
async fn a_bud_with_no_agent_is_a_fork_with_no_row_and_no_routing() {
    let f = fx();
    f.lane("sol", &["gh:o/r"]).await;
    let out = f
        .graph
        .apply(&bud(headless("spike"), Seq(3)))
        .await
        .expect("a headless bud applies");
    assert_eq!(out.kind, OpKind::Fork);
    assert!(out.rows_written.is_empty(), "a fork writes no row");
    assert!(
        out.digests.is_empty(),
        "a fork has no row to carry a digest, and `digest_on_fork` is false in the tree"
    );
    assert!(f.row("spike").await.is_none());

    // The trajectory and the edge ARE there: a fork is a real branch, just an anonymous one.
    let edges = f.ledger.0.edges(&traj("spike")).await.expect("readable");
    assert_eq!(edges.len(), 1);
    assert_eq!(
        f.steps(&traj("spike")).await[0].kind.as_str(),
        "fork/end-seed"
    );

    let step = &f.steps_of_kind(GRAPH_BUD).await[0];
    let body: GraphBud = serde_json::from_value((*step.body).clone()).expect("a GraphBud");
    assert_eq!(body.agent, None, "a fork is a bud with `agent: None`");
    assert!(body.routing_refs.is_empty(), "and it takes no routing");
    // The parent kept everything: nothing was routed away.
    assert_eq!(f.row("sol").await.unwrap().routing_refs, refs(&["gh:o/r"]));
}

#[tokio::test]
async fn promoting_a_fork_is_adding_the_row_and_nothing_else() {
    let f = fx();
    f.lane("sol", &["gh:o/r"]).await;
    f.graph
        .apply(&bud(headless("spike"), Seq(3)))
        .await
        .expect("applies");

    let edges_before = f.ledger.0.edges(&traj("spike")).await.expect("readable");
    let steps_before = f.steps(&traj("spike")).await;
    let rollups_before = f.rollups_on(&traj("spike")).await;

    // Promotion: one `agents` row over the trajectory that is already there.
    f.ledger
        .0
        .put_agent(AgentRow {
            name: AgentName::new("spike"),
            traj: traj("spike"),
            routing_refs: Default::default(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("the row lands");

    assert!(f.row("spike").await.is_some());
    assert_eq!(
        f.ledger.0.edges(&traj("spike")).await.unwrap(),
        edges_before
    );
    assert_eq!(f.steps(&traj("spike")).await, steps_before);
    assert_eq!(
        f.rollups_on(&traj("spike")).await.len(),
        rollups_before.len()
    );
    // And it is now a member in its own right, reading the parent's past through ancestry.
    let c = f
        .ledger
        .0
        .connected(&AgentName::new("spike"))
        .await
        .expect("readable");
    assert!(c.ancestry.contains(&traj("sol")));
}

#[tokio::test]
async fn a_bud_takes_the_refs_it_claims_and_the_parent_keeps_the_rest() {
    let f = fx();
    f.lane("sol", &["gh:o/r", "gh:o/other"]).await;
    f.graph
        .apply(&bud(lane_child("scout", &["gh:o/r"]), Seq(3)))
        .await
        .expect("applies");
    assert_eq!(
        f.row("scout").await.unwrap().routing_refs,
        refs(&["gh:o/r"]),
        "the child took exactly the ref it claimed"
    );
    assert_eq!(
        f.row("sol").await.unwrap().routing_refs,
        refs(&["gh:o/other"]),
        "the parent kept the rest and lost the reassigned one"
    );
}
