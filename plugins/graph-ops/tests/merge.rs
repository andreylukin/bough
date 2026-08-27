//! §3/§4's merge: two lanes into ONE new head, one reconciliation digest, the union of the refs,
//! the SURVIVOR's overrides, the losing ROW deleted and both TRAJECTORIES kept.

mod common;

use bough_plugin_graph_ops::{GraphMerge, GraphOps, MergeRequest, OpKind, OpRequest, GRAPH_MERGE};
use bough_plugin_ledger::{AgentName, EdgeKind, NewRollup, RollupId, RollupKind, Seq, TrajId};
use bough_plugin_rollups::Attribution;
use common::{base, fx, refs, traj, Fx};

fn merge(survivor: &str, absorbed: &str) -> OpRequest {
    OpRequest::Merge(MergeRequest {
        survivor: AgentName::new(survivor),
        absorbed: AgentName::new(absorbed),
        reason: "one lane is enough".into(),
        by: Attribution::Andrey,
        cites: vec![],
        at: base(),
    })
}

fn head() -> TrajId {
    bough_plugin_graph_ops::merge_head(&traj("sol"), &traj("terra"))
}

async fn two_lanes(f: &Fx) {
    f.lane("sol", &["gh:o/r"]).await;
    f.lane("terra", &["slack:c1"]).await;
}

#[tokio::test]
async fn one_new_head_and_two_merge_edges() {
    let f = fx();
    two_lanes(&f).await;
    let out = f.graph.apply(&merge("sol", "terra")).await.expect("merges");
    assert_eq!(out.kind, OpKind::Merge);
    assert_eq!(out.trajs, vec![head()]);

    let edges = f.ledger.0.edges(&head()).await.expect("readable");
    let merges: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Merge).collect();
    assert_eq!(merges.len(), 2, "one merge edge per parent, and no more");
    assert!(merges.iter().any(|e| e.parent == traj("sol")));
    assert!(merges.iter().any(|e| e.parent == traj("terra")));
    // The surviving ROW points at the new head.
    assert_eq!(f.row("sol").await.unwrap().traj, head());
}

#[tokio::test]
async fn one_reconciliation_digest_spanning_both_parents() {
    let f = fx();
    two_lanes(&f).await;
    let out = f.graph.apply(&merge("sol", "terra")).await.expect("merges");
    assert_eq!(out.digests.len(), 1);

    let calls = f.digests.calls();
    assert_eq!(calls.len(), 1, "a merge costs exactly one model call");
    assert!(calls[0].reconcile, "P5-D13: a merge RECONCILES");
    assert_eq!(calls[0].parents, vec![traj("sol"), traj("terra")]);

    let rows = f.rollups_on(&head()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, RollupKind::Reconciliation);
    assert_eq!(rows[0].src_trajs, vec![traj("sol"), traj("terra")]);

    // And the cited step names it.
    let step = &f.steps_of_kind(GRAPH_MERGE).await[0];
    let body: GraphMerge = serde_json::from_value((*step.body).clone()).expect("a GraphMerge");
    assert_eq!(body.reconciliation, rows[0].id);
    assert_eq!(step.class, bough_plugin_ledger::Class::Evidence);
    assert!(!step.cites.is_empty(), "a cited merge event");
}

#[tokio::test]
async fn routing_refs_are_unioned() {
    let f = fx();
    two_lanes(&f).await;
    f.graph.apply(&merge("sol", "terra")).await.expect("merges");
    assert_eq!(
        f.row("sol").await.unwrap().routing_refs,
        refs(&["gh:o/r", "slack:c1"]),
        "§3: the union, always — no ref is dropped by a merge"
    );
}

#[tokio::test]
async fn overrides_come_from_the_survivor() {
    let f = fx();
    let mut sol = f.lane("sol", &["gh:o/r"]).await;
    let mut terra = f.lane("terra", &["slack:c1"]).await;
    sol.model_override = Some("claude-haiku-4-5-20251001".into());
    sol.tick_floor = Some(std::time::Duration::from_secs(60));
    terra.model_override = Some("some-other-model".into());
    terra.tick_floor = Some(std::time::Duration::from_secs(5));
    terra.wake_classes = ["deploy".to_string()].into_iter().collect();
    f.ledger.0.put_agent(sol).await.unwrap();
    f.ledger.0.put_agent(terra).await.unwrap();

    f.graph.apply(&merge("sol", "terra")).await.expect("merges");
    let row = f.row("sol").await.unwrap();
    assert_eq!(
        row.model_override.as_deref(),
        Some("claude-haiku-4-5-20251001")
    );
    assert_eq!(row.tick_floor, Some(std::time::Duration::from_secs(60)));
    // Wake classes UNION: an urgency either lane honoured, the survivor honours.
    assert!(row.wake_classes.contains("ask") && row.wake_classes.contains("deploy"));
}

#[tokio::test]
async fn the_losing_row_is_deleted() {
    let f = fx();
    two_lanes(&f).await;
    let out = f.graph.apply(&merge("sol", "terra")).await.expect("merges");
    assert_eq!(out.rows_deleted, vec![AgentName::new("terra")]);
    assert!(f.row("terra").await.is_none(), "the ROW goes");
    // …and its trajectory does not.
    assert!(!f.steps(&traj("terra")).await.is_empty());
}

#[tokio::test]
async fn both_trajectories_still_read_after_the_merge() {
    let f = fx();
    two_lanes(&f).await;
    let sol_before = f.steps(&traj("sol")).await;
    let terra_before = f.steps(&traj("terra")).await;

    f.graph.apply(&merge("sol", "terra")).await.expect("merges");

    // Byte-for-byte: a merge deletes nothing from the past and moves no step.
    assert_eq!(f.steps(&traj("sol")).await, sol_before);
    assert_eq!(f.steps(&traj("terra")).await, terra_before);
    // And the surviving agent's membership reaches BOTH of them — a merged lane that could not
    // read its own history would have lost it.
    let c = f
        .ledger
        .0
        .connected(&AgentName::new("sol"))
        .await
        .expect("membership is derived at need");
    let trajs = c.trajectories();
    assert!(trajs.contains(&traj("sol")), "{trajs:?}");
    assert!(trajs.contains(&traj("terra")), "{trajs:?}");
}

#[tokio::test]
async fn every_sealed_tier_stays_valid() {
    let f = fx();
    two_lanes(&f).await;
    // A sealed tier on each parent, as Phase 4 would have left it.
    for name in ["sol", "terra"] {
        f.ledger
            .0
            .seal_rollup(NewRollup {
                id: Some(RollupId::new(format!("tier:lane/{name}:1:1-6"))),
                traj: traj(name),
                kind: RollupKind::Tier,
                tier: 1,
                from_seq: Seq(1),
                to_seq: Seq(6),
                src_trajs: vec![traj(name)],
                body: serde_json::json!({ "text": "the episode" }),
                notable_refs: Default::default(),
                prompt_ver: "r4.1".into(),
                sealed_at: base(),
            })
            .await
            .expect("the tier seals");
    }
    let before: Vec<_> = f
        .rollups_on(&traj("sol"))
        .await
        .into_iter()
        .chain(f.rollups_on(&traj("terra")).await)
        .collect();

    f.graph.apply(&merge("sol", "terra")).await.expect("merges");

    let after: Vec<_> = f
        .rollups_on(&traj("sol"))
        .await
        .into_iter()
        .chain(f.rollups_on(&traj("terra")).await)
        .collect();
    // Neither trajectory moved, so no sealed block was superseded, re-ranged or re-sealed: every
    // tier still covers exactly the seqs it covered.
    assert_eq!(after, before);
    for r in &after {
        assert!(r.superseded_by.is_none(), "`{}` was superseded", r.id);
        assert!(r.to_seq <= f.ledger.0.head_seq(&r.traj).await.unwrap().unwrap());
    }
}

#[tokio::test]
async fn a_merge_with_no_survivor_named_is_a_leader_question() {
    let f = fx();
    two_lanes(&f).await;
    let err = f
        .graph
        .apply(&merge("", "terra"))
        .await
        .expect_err("a merge without Andrey's choice is refused");
    assert!(
        matches!(err, bough_plugin_graph_ops::GraphError::NoSurvivor),
        "{err}"
    );
    // The question was ASKED, not guessed.
    let asked = f.ask.asked();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].about.contains("terra"), "{}", asked[0].about);
    // And nothing was written: both rows stand, no head, no digest.
    assert!(f.row("sol").await.is_some() && f.row("terra").await.is_some());
    assert!(f.digests.calls().is_empty());
    assert!(f.steps_of_kind(GRAPH_MERGE).await.is_empty());
}
