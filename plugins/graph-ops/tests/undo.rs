//! §4's undo rules. An UNUSED split undoes as POINTERS and calls no model at all; a LIVED-IN one
//! undoes as a MERGE, which reconciles and leaves the divergent heads behind by construction.

mod common;

use bough_plugin_graph_ops::{
    ChildSpec, GraphOps, GraphUndo, OpKind, OpRequest, SplitRequest, UndoRequest, UndoShape,
    GRAPH_UNDO,
};
use bough_plugin_ledger::{AgentName, Class, StepId, WakeId};
use bough_plugin_rollups::Attribution;
use common::{base, fx, refs, traj, Fx};

fn child(name: &str, rs: &[&str]) -> ChildSpec {
    ChildSpec {
        agent: Some(AgentName::new(name)),
        traj: traj(name),
        routing_refs: refs(rs),
        wake_classes: Default::default(),
    }
}

fn split() -> OpRequest {
    OpRequest::Split(SplitRequest {
        parent: AgentName::new("sol"),
        at_seq: None,
        children: vec![child("a", &["gh:o/r"]), child("b", &["slack:c1"])],
        reason: "two concerns".into(),
        by: Attribution::Andrey,
        cites: vec![],
        at: base(),
    })
}

fn undo(of: StepId) -> UndoRequest {
    UndoRequest {
        of,
        by: Attribution::Andrey,
        at: base(),
    }
}

/// A split, applied. Returns the `graph/split` step id.
async fn a_split(f: &Fx) -> StepId {
    f.lane("sol", &["gh:o/r", "slack:c1", "mail:ops"]).await;
    f.graph.apply(&split()).await.expect("applies").step
}

/// Give a child a life of its own: one closed wake beyond its end-seed.
async fn live_in(f: &Fx, name: &str) {
    let t = traj(name);
    let w = WakeId::new(format!("{name}/w0"));
    f.append(
        &t,
        &w,
        "wake/start",
        Class::Thought,
        serde_json::json!({ "urgency": "coalesced" }),
        vec![],
    )
    .await;
    f.append(
        &t,
        &w,
        "pin/set",
        Class::Thought,
        serde_json::json!({ "title": "work", "text": "done here" }),
        vec![],
    )
    .await;
    f.append(
        &t,
        &w,
        "wake/end",
        Class::Thought,
        serde_json::json!({ "reason": "completed" }),
        vec![],
    )
    .await;
}

#[tokio::test]
async fn undoing_an_unused_split_is_pointers_only() {
    let f = fx();
    let step = a_split(&f).await;
    let out = f.graph.undo(&undo(step.clone())).await.expect("undoes");

    assert_eq!(out.kind, OpKind::Undo);
    assert_eq!(out.undo_shape, Some(UndoShape::Pointers));
    assert_eq!(out.edges, 0, "pointers only: no edge is written or removed");
    // The child ROWS are gone; their trajectories are not (nothing is ever deleted).
    assert!(f.row("a").await.is_none() && f.row("b").await.is_none());
    assert!(!f.steps(&traj("a")).await.is_empty());
    // The parent's refs are restored from the op step: exactly what it had before the split.
    assert_eq!(
        f.row("sol").await.unwrap().routing_refs,
        refs(&["gh:o/r", "slack:c1", "mail:ops"])
    );
    let undo_step = &f.steps_of_kind(GRAPH_UNDO).await[0];
    let body: GraphUndo = serde_json::from_value((*undo_step.body).clone()).expect("a GraphUndo");
    assert_eq!(body.of, step);
    assert_eq!(body.shape, UndoShape::Pointers);
}

#[tokio::test]
async fn an_unused_undo_writes_no_digest_and_calls_no_model() {
    let f = fx();
    let step = a_split(&f).await;
    let calls_after_split = f.digests.calls().len();
    assert_eq!(calls_after_split, 2, "the split's two inheritance digests");

    let out = f.graph.undo(&undo(step)).await.expect("undoes");
    assert!(out.digests.is_empty());
    assert_eq!(
        f.digests.calls().len(),
        calls_after_split,
        "an unused undo summarises NOTHING: there is nothing to reconcile"
    );
}

#[tokio::test]
async fn undoing_a_lived_in_split_is_a_merge() {
    let f = fx();
    let step = a_split(&f).await;
    live_in(&f, "a").await;

    let out = f.graph.undo(&undo(step)).await.expect("undoes");
    assert_eq!(out.undo_shape, Some(UndoShape::Merge));
    // The merge path ran: one reconciliation digest, the absorbed row deleted.
    assert_eq!(out.digests.len(), 1);
    let recon = f
        .digests
        .calls()
        .into_iter()
        .find(|c| c.reconcile)
        .expect("a reconciliation digest was requested");
    assert_eq!(recon.parents, vec![traj("sol"), traj("a")]);
    assert!(
        f.row("a").await.is_none(),
        "the lived-in child was absorbed"
    );
    // The never-used sibling went by the pointer rule in the same undo.
    assert!(f.row("b").await.is_none());
    // The parent survives, holding the union.
    let sol = f.row("sol").await.unwrap();
    assert!(sol
        .routing_refs
        .contains(&bough_plugin_ledger::Ref::new("gh:o/r")));
}

#[tokio::test]
async fn divergent_heads_are_left_behind_and_named_in_the_undo_step() {
    let f = fx();
    let step = a_split(&f).await;
    live_in(&f, "a").await;
    let a_before = f.steps(&traj("a")).await;

    f.graph.undo(&undo(step.clone())).await.expect("undoes");

    // The divergent head is LEFT BEHIND, byte for byte: no trajectory is ever deleted.
    assert_eq!(f.steps(&traj("a")).await, a_before);
    let undo_step = f
        .steps_of_kind(GRAPH_UNDO)
        .await
        .into_iter()
        .next()
        .expect("one undo step");
    let body: GraphUndo = serde_json::from_value((*undo_step.body).clone()).expect("a GraphUndo");
    assert_eq!(body.shape, UndoShape::Merge);
    assert!(
        body.trajs.contains(&traj("a")) && body.trajs.contains(&traj("b")),
        "the undo NAMES what it left behind: {:?}",
        body.trajs
    );
    assert_eq!(body.of, step);
    assert_eq!(undo_step.class, Class::Evidence);
}
