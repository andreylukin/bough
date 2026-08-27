//! §10, P5-D12 — the pinned prefix, end to end. What this file pins:
//!
//! * the request the CHILD's adapter receives carries the PARENT's assembled prefix at the fork
//!   seq, byte for byte — not the child's own projection;
//! * the child's `request/header` records that prefix's digest, so §0.2's reconstruction anchor
//!   is the parent's;
//! * the child's chain carries one `fork/prefix` naming the parent and the seq;
//! * re-assembling the parent AT THAT SEQ reproduces the pinned bytes, which is the whole reason
//!   the anchor is worth writing.

use crate::support;

use bough_plugin_agents::AgentId;
use bough_plugin_ledger::{AgentName, Seq, StepId, WakeId};
use bough_plugin_projection::{AssembleRequest, Projector};
use bough_plugin_workers::{AskMode, Bounds, SealSpec, StartWorker, WorkerKind, WorkerOutcome};
use support::*;

fn bounds() -> Bounds {
    Bounds {
        max_in_flight: 4,
        max_depth: 3,
        per_wake_spawn_cap: 4,
    }
}

fn fork_req(wake: &str, task: &str) -> StartWorker {
    StartWorker {
        kind: WorkerKind::Fork,
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

/// One finished fork: a parent with a closed wake behind it, then a fork that reports.
struct Forked {
    f: Fixture,
    child_traj: bough_plugin_ledger::TrajId,
    at_seq: Seq,
    /// The system prefix the CHILD's request carried.
    child_system: String,
}

async fn forked() -> Forked {
    let f = Fixture::mounted(bounds()).await;
    f.mount_fork(8).await;
    let (agent, disposer) = f.parent_agent().await;
    f.adapter
        .script(vec![says("the parent's own answer, at some length")]);
    f.run_parent_wake(&agent, "what is the state of the tree?")
        .await;

    // From here the fork's child is the only thing that talks to the adapter.
    let before = f.adapter.requests().len();
    f.adapter.script(vec![reports(
        "forked and looked",
        "the tree is clean",
        "step:s1",
    )]);
    let result = f
        .workers
        .start(&f.ctx, fork_req("wk-fork", "look at the tree"))
        .await
        .expect("the fork runs");
    assert_eq!(result.outcome, WorkerOutcome::Done, "{:?}", result.outcome);

    let child_traj = bough_plugin_worker_fork::fork_traj(&result.worker);
    let steps = f.steps_of(&child_traj).await;
    let anchor = steps
        .iter()
        .find(|s| s.kind.as_str() == bough_plugin_worker_fork::FORK_PREFIX)
        .expect("the child's chain records where its prefix came from");
    let at_seq = Seq(anchor.body["as_of"].as_u64().expect("a seq"));

    let child_system = f
        .adapter
        .requests()
        .into_iter()
        .nth(before)
        .expect("the child sent a request")
        .system
        .expect("with a system prefix");

    let _ = disposer;
    Forked {
        f,
        child_traj,
        at_seq,
        child_system,
    }
}

#[tokio::test]
async fn the_childs_system_prefix_equals_the_parents_at_the_fork_seq() {
    let it = forked().await;
    let parents =
        it.f.assembler
            .assemble(&AssembleRequest {
                agent: parent(),
                wake: None,
                at: now(),
                budget: None,
                as_of: Some(it.at_seq),
            })
            .await
            .expect("the parent still assembles at the fork seq");
    assert_eq!(
        it.child_system,
        parents.to_text(),
        "the child assembled a projection of its own instead of replaying the parent's"
    );
    assert!(
        !it.child_system.contains("worker-fork-"),
        "the child's own identity leaked into the pinned prefix:\n{}",
        it.child_system
    );
}

#[tokio::test]
async fn the_request_header_digest_matches_the_parents() {
    let it = forked().await;
    let parents =
        it.f.assembler
            .assemble(&AssembleRequest {
                agent: parent(),
                wake: None,
                at: now(),
                budget: None,
                as_of: Some(it.at_seq),
            })
            .await
            .expect("the parent still assembles");
    let steps = it.f.steps_of(&it.child_traj).await;
    let recorded = bough_plugin_worker_fork::invariant::newest_header_digest(&steps)
        .expect("the child appended a request/header");
    assert_eq!(
        recorded,
        bough_plugin_worker_fork::invariant::digest(&parents.to_text()),
        "the anchor §0.2 reconstructs from does not describe the parent's prefix"
    );
}

#[tokio::test]
async fn the_fork_prefix_step_names_the_parent_and_the_seq() {
    let it = forked().await;
    let steps = it.f.steps_of(&it.child_traj).await;
    let anchors = bough_plugin_worker_fork::invariant::anchors(&steps);
    assert_eq!(anchors.len(), 1, "exactly one anchor per fork");
    assert_eq!(anchors[0].of_agent, AgentName::new("sol"));
    assert_eq!(anchors[0].as_of, it.at_seq);
    let row = steps
        .iter()
        .find(|s| s.kind.as_str() == bough_plugin_worker_fork::FORK_PREFIX)
        .expect("the row");
    assert_eq!(
        row.class,
        bough_plugin_ledger::Class::Thought,
        "an anchor asserts nothing about the world"
    );
}

#[tokio::test]
async fn re_assembling_the_parent_at_that_seq_reproduces_the_pin() {
    let it = forked().await;
    // Twice, and after the fork is long gone: reconstruction is a property of the LEDGER, not of
    // anything the run left in memory.
    for _ in 0..2 {
        let again =
            it.f.assembler
                .assemble(&AssembleRequest {
                    agent: parent(),
                    wake: None,
                    at: now(),
                    budget: None,
                    as_of: Some(it.at_seq),
                })
                .await
                .expect("the parent still assembles at the fork seq");
        assert_eq!(again.to_text(), it.child_system);
    }
}
