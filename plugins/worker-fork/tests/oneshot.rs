//! §10, §4 — the one-shot fork. What this file pins:
//!
//! * the child is handed the parent's history: the request it sends carries the parent's own
//!   wake, verbatim, because the pinned prefix IS the parent's projection;
//! * its report lands in the SPAWNER's chain as cited evidence, through the same path a spawned
//!   worker's report takes;
//! * one report and the child is gone — the agent is disposed and its pin with it;
//! * a fork asked for while the parent is mid-wake branches BELOW the open wake; the parent never
//!   pauses (P5-D7);
//! * a fork is not a way around §7's bounds: it counts against the same per-wake cap a spawn does.

use crate::support;

use bough_plugin_agents::AgentId;
use bough_plugin_ledger::{Class, Seq, StepId, WakeId};
use bough_plugin_projection::{AssembleRequest, Projector};
use bough_plugin_workers::{
    AskMode, Bounds, SealSpec, StartWorker, WorkerError, WorkerKind, WorkerOutcome, WorkersHandle,
};
use support::*;

fn bounds(per_wake: usize) -> Bounds {
    Bounds {
        max_in_flight: 4,
        max_depth: 3,
        per_wake_spawn_cap: per_wake,
    }
}

fn req(kind: WorkerKind, wake: &str, task: &str) -> StartWorker {
    StartWorker {
        kind,
        spawner: parent(),
        spawner_id: AgentId::new("a1"),
        wake: WakeId::new(wake),
        step: StepId::new("s0"),
        depth: 1,
        task: task.to_string(),
        seal: SealSpec::report(),
        tools: None,
        ask_mode: AskMode::End,
        at: now(),
    }
}

const PARENTS_ANSWER: &str = "the parent said this, and the fork must be able to read it";

#[tokio::test]
async fn the_child_sees_the_parents_message_history() {
    let f = Fixture::mounted(bounds(4)).await;
    f.mount_fork(8).await;
    let (agent, _d) = f.parent_agent().await;
    f.adapter.script(vec![says(PARENTS_ANSWER)]);
    f.run_parent_wake(&agent, "a question only the parent was asked")
        .await;

    let before = f.adapter.requests().len();
    f.adapter
        .script(vec![reports("looked", "it is clean", "step:s1")]);
    f.workers
        .start(&f.ctx, req(WorkerKind::Fork, "wk1", "continue from here"))
        .await
        .expect("the fork runs");

    let child = f
        .adapter
        .requests()
        .into_iter()
        .nth(before)
        .expect("the child sent a request");
    // The parent's history lands in the projection's TAIL band, which since the §12 tier split
    // rides `system_volatile`; both tiers together are what the child was shown.
    let system = format!(
        "{}\n{}",
        child.system.expect("with a system prefix"),
        child.system_volatile.unwrap_or_default()
    );
    assert!(
        system.contains(PARENTS_ANSWER),
        "the parent's own wake is not in what the child was shown:\n{system}"
    );
    assert!(
        system.contains("a question only the parent was asked"),
        "the parent's mail is not in what the child was shown:\n{system}"
    );
    // And the task itself reached the child as its seed message.
    let text = format!("{:?}", child.messages);
    assert!(text.contains("continue from here"), "{text}");
}

#[tokio::test]
async fn the_report_lands_as_cited_evidence_in_the_spawner_chain() {
    let f = Fixture::mounted(bounds(4)).await;
    f.mount_fork(8).await;
    let (agent, _d) = f.parent_agent().await;
    f.adapter.script(vec![says(PARENTS_ANSWER)]);
    f.run_parent_wake(&agent, "go").await;

    f.adapter.script(vec![reports(
        "read the tree",
        "src/lib.rs line 3 reads `bar`",
        "step:evidence-1",
    )]);
    let result = f
        .workers
        .start(&f.ctx, req(WorkerKind::Fork, "wk1", "look"))
        .await
        .expect("the fork runs");
    assert_eq!(result.outcome, WorkerOutcome::Done);

    let steps = f.steps_of(&parent_traj()).await;
    let report = steps
        .iter()
        .find(|s| s.kind.as_str() == "worker/report")
        .expect("the report is in the SPAWNER's chain");
    assert_eq!(
        report.class,
        Class::Evidence,
        "a report with an external cite is evidence"
    );
    assert_eq!(report.cites.len(), 1);
    assert_eq!(report.cites[0].r#ref.as_str(), "step:evidence-1");
    assert_eq!(report.body["summary"], "read the tree");
    assert_eq!(
        Some(&report.id),
        result.report_step.as_ref(),
        "and the result names it, so the spawner's next claim can cite it"
    );
}

#[tokio::test]
async fn the_child_is_disposed_after_one_report() {
    let f = Fixture::mounted(bounds(4)).await;
    f.mount_fork(8).await;
    let (agent, _d) = f.parent_agent().await;
    f.adapter.script(vec![says(PARENTS_ANSWER)]);
    f.run_parent_wake(&agent, "go").await;

    f.adapter
        .script(vec![reports("done", "it is clean", "step:s1")]);
    let result = f
        .workers
        .start(&f.ctx, req(WorkerKind::Fork, "wk1", "look"))
        .await
        .expect("the fork runs");

    let name = WorkersHandle::worker_agent_name(&parent(), &result.worker);
    assert!(
        f.agents.by_name(&name).is_none(),
        "the fork outlived its one report"
    );
    // And the PIN went with it: nothing global remembers a fork's prefix (P5-D12).
    let after = f
        .assembler
        .assemble(&AssembleRequest {
            agent: name.clone(),
            wake: None,
            at: now(),
            budget: None,
            as_of: None,
        })
        .await
        .expect("an answer wake must always be buildable");
    assert!(
        !after.to_text().contains(PARENTS_ANSWER),
        "the pin survived the agent that held it:\n{}",
        after.to_text()
    );
}

#[tokio::test]
async fn a_fork_inside_an_open_wake_branches_below_it() {
    let f = Fixture::mounted(bounds(4)).await;
    f.mount_fork(8).await;
    let (agent, _d) = f.parent_agent().await;
    f.adapter.script(vec![says(PARENTS_ANSWER)]);
    f.run_parent_wake(&agent, "go").await;

    // The parent is now mid-wake: a `wake/start` with no `wake/end` after it. The fork must not
    // wait for it and must not clip into it.
    let closed_head = f
        .steps_of(&parent_traj())
        .await
        .last()
        .expect("the parent has a chain")
        .seq;
    f.append(
        &parent_traj(),
        "w-open",
        "wake/start",
        serde_json::json!({ "urgency": "immediate", "trigger": null, "claimed": [] }),
    )
    .await;
    f.append(
        &parent_traj(),
        "w-open",
        "thought/text",
        serde_json::json!({ "text": "mid-wake, and still going", "step_index": 0 }),
    )
    .await;

    f.adapter
        .script(vec![reports("done", "it is clean", "step:s1")]);
    let result = f
        .workers
        .start(&f.ctx, req(WorkerKind::Fork, "wk1", "look"))
        .await
        .expect("a fork never waits for the parent's open wake");

    let child_traj = bough_plugin_worker_fork::fork_traj(&result.worker);
    let anchors = bough_plugin_worker_fork::invariant::anchors(&f.steps_of(&child_traj).await);
    assert_eq!(
        anchors[0].as_of, closed_head,
        "the fork branched at {:?} rather than below the open wake at {closed_head:?}",
        anchors[0].as_of
    );
    assert!(
        anchors[0].as_of < Seq(closed_head.0 + 1),
        "and never inside it"
    );
    // The parent's open wake is untouched: nothing paused it and nothing closed it.
    let ends = f
        .steps_of(&parent_traj())
        .await
        .into_iter()
        .filter(|s| s.kind.as_str() == "wake/end" && s.wake.as_str() == "w-open")
        .count();
    assert_eq!(ends, 0, "the parent's wake was ended by the fork");
}

#[tokio::test]
async fn the_fork_bound_counts_against_the_same_spawn_bounds() {
    // ONE worker per wake, whatever kind it is.
    let f = Fixture::mounted(bounds(1)).await;
    f.mount_fork(8).await;
    f.mount_spawn(8).await;
    let (agent, _d) = f.parent_agent().await;
    f.adapter.script(vec![says(PARENTS_ANSWER)]);
    f.run_parent_wake(&agent, "go").await;

    f.adapter.script(vec![says("a worker that never reports")]);
    f.workers
        .start(&f.ctx, req(WorkerKind::Spawn, "wk1", "one"))
        .await
        .expect("the first worker of the wake is allowed");

    let err = f
        .workers
        .start(&f.ctx, req(WorkerKind::Fork, "wk1", "two"))
        .await
        .expect_err("a fork is not a way around the wake's spawn cap");
    match err {
        WorkerError::BoundsExceeded { bound, limit, .. } => {
            assert_eq!(bound, "per_wake_spawn_cap");
            assert_eq!(limit, 1);
        }
        other => panic!("the refusal names the wrong bound: {other}"),
    }
}
