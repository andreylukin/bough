//! §4: ambiguous routing becomes a leader QUESTION, never a guess — and nothing is written while
//! the question is open.

use crate::common;

use bough_plugin_graph_ops::{ChildSpec, GraphError, GraphOps, OpRequest, SplitRequest};
use bough_plugin_ledger::{AgentName, Ref};
use bough_plugin_rollups::Attribution;
use common::{base, fx, fx_with_failing_ask, refs, traj};

fn contested() -> OpRequest {
    let child = |name: &str| ChildSpec {
        agent: Some(AgentName::new(name)),
        traj: traj(name),
        // BOTH children claim the same ref. Nobody but Andrey may settle it.
        routing_refs: refs(&["gh:o/r"]),
        wake_classes: Default::default(),
    };
    OpRequest::Split(SplitRequest {
        parent: AgentName::new("sol"),
        at_seq: None,
        children: vec![child("lane-a"), child("lane-b")],
        reason: "two concerns".into(),
        by: Attribution::Andrey,
        cites: vec![],
        at: base(),
    })
}

#[tokio::test]
async fn an_ambiguous_split_produces_a_leader_question() {
    let f = fx();
    f.lane("sol", &["gh:o/r"]).await;
    let err = f
        .graph
        .apply(&contested())
        .await
        .expect_err("a contested ref is never settled by the planner");
    assert!(matches!(err, GraphError::Ambiguous { .. }), "{err}");

    let asked = f.ask.asked();
    assert_eq!(asked.len(), 1, "one question, once");
    assert!(asked[0].about.contains("gh:o/r"), "{}", asked[0].about);
    // Both claimants BY NAME, and the names are checked as words: `contains('a')` would be
    // satisfied by any English sentence, which is what this assertion used to be.
    for claimant in ["lane-a", "lane-b"] {
        assert!(
            asked[0].about.contains(claimant),
            "the question names BOTH claimants; `{claimant}` is missing from: {}",
            asked[0].about
        );
    }
    // One option per contested REF (there is one), and it is the question itself: the leader is
    // asked to settle the ref, not to pick a lane from a menu.
    assert_eq!(asked[0].options.len(), 1, "{:?}", asked[0].options);
    assert_eq!(asked[0].asked_by, "graph-ops");

    // `plan()` alone says the same thing, and asks nobody: it is pure with respect to the world.
    let plan = f.graph.plan(&contested()).await.expect("a plan is total");
    assert_eq!(plan.questions.len(), 1);
    assert_eq!(f.ask.asked().len(), 1, "planning asked nothing further");
}

#[tokio::test]
async fn no_split_is_written_while_the_question_is_open() {
    let f = fx();
    let parent = f.lane("sol", &["gh:o/r"]).await;
    let before = f.steps(&parent.traj).await;

    f.graph.apply(&contested()).await.expect_err("refused");

    // Not one trajectory, edge, row, digest or step.
    assert_eq!(f.steps(&parent.traj).await, before);
    assert!(f.steps(&traj("lane-a")).await.is_empty());
    assert!(f.steps(&traj("lane-b")).await.is_empty());
    assert!(f.row("lane-a").await.is_none() && f.row("lane-b").await.is_none());
    assert!(f.digests.calls().is_empty(), "no model was called");
    assert!(f.steps_of_kind("graph/split").await.is_empty());
    // And the parent still holds every ref it held.
    assert_eq!(f.row("sol").await.unwrap().routing_refs, refs(&["gh:o/r"]));
}

#[tokio::test]
async fn the_question_is_wake_class_mail() {
    let f = fx();
    f.lane("sol", &["gh:o/r"]).await;
    f.graph.apply(&contested()).await.expect_err("refused");

    let asked = f.ask.asked();
    assert!(
        asked[0]
            .refs
            .contains(&Ref::new(bough_plugin_mail_router::ASK_CLASS_REF)),
        "P5-D3: a question only Andrey can settle carries `class:ask`, which is what reactivates \
         a dormant leader — refs were {:?}",
        asked[0].refs
    );
    assert_eq!(
        asked[0].at,
        base(),
        "the clock is injected, never read here"
    );
}

/// §4: the refusal is the rule, and asking is how the rule reaches Andrey. An ask that FAILS
/// still refuses the op — and it surfaces the ask failure rather than reporting a tidy
/// "ambiguous" about a question nobody ever received.
///
/// This replaces the `question_on_ambiguity: false` test seam, which was a production config
/// field whose only purpose was to shorten this test: setting it in any patch layer silently
/// removed §4's notification path (P5-D9, withdrawn).
#[tokio::test]
async fn an_ask_that_fails_still_refuses_and_is_not_swallowed() {
    let f = fx_with_failing_ask();
    f.lane("sol", &["gh:o/r"]).await;
    let err = f
        .graph
        .apply(&contested())
        .await
        .expect_err("still refused");
    assert!(
        err.to_string().contains("the mail seam is down"),
        "the ask failure reaches the caller: {err}"
    );
    // And nothing was written while the question was open.
    assert!(f.steps(&traj("lane-a")).await.is_empty());
    assert!(f.row("lane-a").await.is_none(), "no row was written");
}
