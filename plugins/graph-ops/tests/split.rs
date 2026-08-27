//! §4's split: two heads from one, at the parent's head. What a split must leave behind is two
//! ancestor edges, two end-seed markers, one inheritance digest per child, a routing partition
//! that loses nothing, and — LAST — one cited step naming all of it (P5-D8).

mod common;

use bough_plugin_graph_ops::{
    ChildSpec, GraphOps, GraphSplit, OpKind, OpRequest, SplitRequest, GRAPH_SPLIT,
};
use bough_plugin_ledger::{AgentName, EdgeKind, Ref, Seq};
use bough_plugin_rollups::Attribution;
use common::{base, fx, refs, traj};

fn child(name: &str, rs: &[&str]) -> ChildSpec {
    ChildSpec {
        agent: Some(AgentName::new(name)),
        traj: traj(name),
        routing_refs: refs(rs),
        wake_classes: ["ask".to_string()].into_iter().collect(),
    }
}

fn split(children: Vec<ChildSpec>) -> OpRequest {
    OpRequest::Split(SplitRequest {
        parent: AgentName::new("sol"),
        at_seq: None,
        children,
        reason: "two concerns, two lanes".into(),
        by: Attribution::Andrey,
        cites: vec![],
        at: base(),
    })
}

#[tokio::test]
async fn a_split_writes_two_ancestor_edges_and_two_end_seeds() {
    let f = fx();
    f.lane("sol", &["gh:o/r", "slack:c1"]).await;
    let out = f
        .graph
        .apply(&split(vec![
            child("a", &["gh:o/r"]),
            child("b", &["slack:c1"]),
        ]))
        .await
        .expect("a settled split applies");
    assert_eq!(out.kind, OpKind::Split);
    assert_eq!(out.edges, 2);

    for name in ["a", "b"] {
        let edges = f.ledger.0.edges(&traj(name)).await.expect("readable");
        let ancestors: Vec<_> = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Ancestor && e.parent == traj("sol"))
            .collect();
        assert_eq!(ancestors.len(), 1, "`{name}` has exactly one ancestor edge");
        assert_eq!(ancestors[0].at_seq, Seq(6), "the parent's resolved head");
        let steps = f.steps(&traj(name)).await;
        assert_eq!(steps[0].kind.as_str(), "fork/end-seed");
        assert_eq!(steps[0].seq, Seq(1));
    }
}

#[tokio::test]
async fn one_inheritance_digest_per_child_naming_src_trajs() {
    let f = fx();
    f.lane("sol", &["gh:o/r", "slack:c1"]).await;
    let out = f
        .graph
        .apply(&split(vec![
            child("a", &["gh:o/r"]),
            child("b", &["slack:c1"]),
        ]))
        .await
        .expect("applies");
    assert_eq!(out.digests.len(), 2, "one digest per child, and no more");

    // Every digest went through `ctx.rollups` — this crate seals nothing itself.
    let calls = f.digests.calls();
    assert_eq!(calls.len(), 2);
    for (call, name) in calls.iter().zip(["a", "b"]) {
        assert_eq!(call.traj, traj(name));
        assert_eq!(
            call.parents,
            vec![traj("sol")],
            "the parent chain it inherits"
        );
        assert!(!call.reconcile, "a split inherits; it does not reconcile");
    }
    // And `src_trajs` on the sealed row names the parent, which is what makes an inheritance
    // digest distinguishable from a standing one in the store.
    for name in ["a", "b"] {
        let rows = f.rollups_on(&traj(name)).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].src_trajs, vec![traj("sol")]);
    }
}

#[tokio::test]
async fn routing_refs_are_reassigned_and_the_parent_keeps_the_rest() {
    let f = fx();
    f.lane("sol", &["gh:o/r", "slack:c1", "mail:ops"]).await;
    f.graph
        .apply(&split(vec![
            child("a", &["gh:o/r"]),
            child("b", &["slack:c1"]),
        ]))
        .await
        .expect("applies");

    assert_eq!(f.row("a").await.unwrap().routing_refs, refs(&["gh:o/r"]));
    assert_eq!(f.row("b").await.unwrap().routing_refs, refs(&["slack:c1"]));
    // The unclaimed ref STAYS with the parent: a split never makes a black hole.
    assert_eq!(
        f.row("sol").await.unwrap().routing_refs,
        refs(&["mail:ops"])
    );
}

#[tokio::test]
async fn the_cited_split_step_is_appended_last_and_names_everything() {
    let f = fx();
    f.lane("sol", &["gh:o/r", "slack:c1"]).await;
    let out = f
        .graph
        .apply(&split(vec![
            child("a", &["gh:o/r"]),
            child("b", &["slack:c1"]),
        ]))
        .await
        .expect("applies");

    let chain = f.steps(&traj("sol")).await;
    let last = chain.last().expect("a chain");
    assert_eq!(last.id, out.step, "the op step is the parent's newest step");
    assert_eq!(last.kind.as_str(), GRAPH_SPLIT);
    assert_eq!(
        last.class,
        bough_plugin_ledger::Class::Evidence,
        "a structure change is a FACT"
    );
    assert!(
        !last.cites.is_empty(),
        "a cited split event: the ledger itself refuses an uncited one"
    );

    let body: GraphSplit = serde_json::from_value((*last.body).clone()).expect("a GraphSplit");
    assert_eq!(body.parent, traj("sol"));
    assert_eq!(body.at_seq, Seq(6));
    assert_eq!(body.children.len(), 2);
    for (rec, name) in body.children.iter().zip(["a", "b"]) {
        assert_eq!(rec.traj, traj(name));
        assert_eq!(rec.agent, Some(AgentName::new(name)));
        assert!(rec.digest.is_some(), "the child's inheritance digest");
        assert!(!rec.routing_refs.is_empty());
    }
    assert_eq!(body.reason, "two concerns, two lanes");

    // LAST also means: everything it names already exists when it lands. The end-seed steps it
    // cites are readable, and each names a trajectory the body names.
    for cite in last.cites.iter() {
        let id = cite.r#ref.as_str().strip_prefix("step:").expect("step ref");
        let seed = f
            .ledger
            .0
            .step(&bough_plugin_ledger::StepId::new(id))
            .await
            .expect("readable")
            .expect("the cited end-seed exists");
        assert!(body.children.iter().any(|c| c.traj == seed.traj));
    }
}

#[tokio::test]
async fn the_past_is_not_partitioned_both_children_still_read_it() {
    let f = fx();
    let parent = f.lane("sol", &["gh:o/r", "slack:c1"]).await;
    let before = f.steps(&parent.traj).await.len();
    f.graph
        .apply(&split(vec![
            child("a", &["gh:o/r"]),
            child("b", &["slack:c1"]),
        ]))
        .await
        .expect("applies");

    // The parent's chain gained the op step and lost NOTHING: §3 partitions the future, never
    // the past.
    let after = f.steps(&parent.traj).await;
    assert_eq!(after.len(), before + 1);
    // And both children reach it — the same past, twice, through ancestry.
    for name in ["a", "b"] {
        let c = f
            .ledger
            .0
            .connected(&AgentName::new(name))
            .await
            .expect("membership is derived at need");
        assert!(
            c.ancestry.contains(&traj("sol")),
            "`{name}` must still read the parent's chain"
        );
        assert!(c.trajectories().contains(&traj("sol")));
    }
    // The refs each child took now also reach the parent's steps by ref match, which is the other
    // half of "the past is shared".
    let _ = Ref::new("gh:o/r");
}
