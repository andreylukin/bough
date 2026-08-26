//! V9 — crash repair (§5). An orphaned trailing wake closes as `interrupted`, a `tool/call` with
//! no result gets `TOOL_OUTCOME_UNKNOWN`, and ROLLUPS ARE NEVER TOUCHED.

mod support;

use bough_plugin_agent_loop::repair;
use bough_plugin_ledger::{
    Append, Class, NewRollup, RollupKind, RollupQuery, Seq, StepQuery, StepType,
};
use support::*;

/// Plant the tail a crash leaves behind: a wake that opened, called a tool, and stopped.
async fn orphaned(f: &Fixture) -> bough_plugin_ledger::WakeId {
    let wake = bough_plugin_ledger::WakeId::new("crashed-wake");
    f.ledger
        .0
        .put_agent(bough_plugin_ledger::AgentRow {
            name: bough_plugin_ledger::AgentName::new("sol"),
            traj: traj(),
            routing_refs: Default::default(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("the row lands");
    for (kind, body) in [
        ("wake/start", serde_json::json!({ "urgency": "immediate" })),
        ("step/start", serde_json::json!({ "index": 0 })),
        (
            "tool/call",
            serde_json::json!({ "call": "c1", "name": "bash", "args": {},
                                "render": "terminal", "step_index": 0 }),
        ),
    ] {
        f.ledger
            .0
            .append(Append {
                traj: traj(),
                wake: wake.clone(),
                kind: StepType::new(kind),
                class: Class::Thought,
                body,
                cites: vec![],
                at: now(),
                id: None,
            })
            .await
            .expect("the step lands");
    }
    wake
}

#[tokio::test]
async fn an_orphaned_trailing_wake_closes_as_interrupted() {
    let f = Fixture::mounted().await;
    let wake = orphaned(&f).await;

    repair::run(&f.ledger, now()).await.expect("repair runs");

    let ends = f
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj()],
            kinds: vec![StepType::new("wake/end")],
            ..Default::default()
        })
        .await
        .expect("a read");
    assert_eq!(ends.len(), 1, "exactly one wake was closed");
    assert_eq!(ends[0].wake, wake);
    assert_eq!(
        ends[0].body["reason"], "interrupted",
        "the one reason no loop emits (§5)"
    );
    assert_eq!(
        ends[0].body["consumed"],
        serde_json::json!([]),
        "a crashed wake consumed nothing"
    );
    // Running repair again is a no-op: the wake is closed now.
    repair::run(&f.ledger, now()).await.expect("repair runs");
    let again = f
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj()],
            kinds: vec![StepType::new("wake/end")],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(again.len(), 1);
}

#[tokio::test]
async fn a_call_without_a_result_gets_tool_outcome_unknown() {
    let f = Fixture::mounted().await;
    orphaned(&f).await;

    repair::run(&f.ledger, now()).await.expect("repair runs");

    let results = f
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj()],
            kinds: vec![StepType::new("tool/result")],
            ..Default::default()
        })
        .await
        .expect("a read");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].body["call"], "c1");
    assert_eq!(
        results[0].body["outcome"], "unknown",
        "TOOL_OUTCOME_UNKNOWN, the one outcome no live pipeline can produce"
    );
    // And it is written BEFORE the wake closes, so no wake ever closes over an unanswered call.
    let ends = f
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj()],
            kinds: vec![StepType::new("wake/end")],
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(results[0].seq < ends[0].seq);
}

#[tokio::test]
async fn repair_never_touches_rollups() {
    let f = Fixture::mounted().await;
    orphaned(&f).await;
    let sealed = f
        .ledger
        .0
        .seal_rollup(NewRollup {
            id: None,
            traj: traj(),
            kind: RollupKind::Tier,
            tier: 0,
            from_seq: Seq(1),
            to_seq: Seq(3),
            src_trajs: vec![traj()],
            body: serde_json::json!({ "text": "a sealed segment" }),
            notable_refs: Default::default(),
            prompt_ver: "p1".into(),
            sealed_at: now(),
        })
        .await
        .expect("the rollup seals");

    repair::run(&f.ledger, now()).await.expect("repair runs");

    let after = f
        .ledger
        .0
        .rollups(&RollupQuery {
            trajs: vec![traj()],
            include_superseded: true,
            ..Default::default()
        })
        .await
        .expect("a read");
    assert_eq!(after.len(), 1, "no rollup was added or removed");
    assert_eq!(after[0].body, sealed.body, "and none was rewritten");
    assert!(after[0].superseded_by.is_none());
}

/// §5's checkpoint-and-answer opens the answer wake BEFORE the interrupted wake closes, so a
/// crash during a preemption leaves TWO wakes open. Both are repaired.
#[tokio::test]
async fn a_crash_during_a_preemption_closes_both_open_wakes() {
    let f = Fixture::mounted().await;
    let interrupted = orphaned(&f).await;
    let answer = bough_plugin_ledger::WakeId::new("answer-wake");
    for (kind, body) in [
        ("wake/start", serde_json::json!({ "urgency": "immediate" })),
        ("step/start", serde_json::json!({ "index": 0 })),
    ] {
        f.ledger
            .0
            .append(Append {
                traj: traj(),
                wake: answer.clone(),
                kind: StepType::new(kind),
                class: Class::Thought,
                body,
                cites: vec![],
                at: now(),
                id: None,
            })
            .await
            .expect("the step lands");
    }

    repair::run(&f.ledger, now()).await.expect("repair runs");

    let ends = f
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj()],
            kinds: vec![StepType::new("wake/end")],
            ..Default::default()
        })
        .await
        .expect("a read");
    let closed: Vec<String> = ends.iter().map(|s| s.wake.to_string()).collect();
    assert!(
        closed.contains(&interrupted.to_string()) && closed.contains(&answer.to_string()),
        "both open wakes close, not only the trailing one; closed = {closed:?}"
    );
    assert!(ends.iter().all(|s| s.body["reason"] == "interrupted"));
}
