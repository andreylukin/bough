//! V1 and V7's "in a booted tree, not just a fixture". `graph-ops`' own suites drive the ops over
//! `ledger-memory`; these two drive them over the SHIPPED rows — a real ledger file, the real
//! `agents` registry, the real `rollups` seam behind the digests — and then ask the kernel what
//! its invariant runner saw.

mod support;

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::Kernel;
use bough_plugin_agents::{Agents, AgentsHandle};
use bough_plugin_graph_ops::{
    BudRequest, ChildSpec, Graph, GraphHandle, MergeRequest, OpKind, OpRequest, SplitRequest,
};
use bough_plugin_hello::trace;
use bough_plugin_ledger::query::{Order, StepQuery};
use bough_plugin_ledger::{
    AgentName, Append, Class, EdgeKind, Ledger, LedgerHandle, Seq, StepType, TrajId, WakeId,
};
use bough_plugin_rollups::Attribution;
use chrono::{TimeZone, Utc};
use support::{boot_real, fixture};

fn at(secs: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + secs, 0)
        .single()
        .expect("a valid instant")
}

fn ledger(kernel: &Kernel) -> Arc<LedgerHandle> {
    kernel
        .root()
        .peek_live::<Ledger>()
        .expect("`ledger` is bound")
}

fn graph(kernel: &Kernel) -> Arc<GraphHandle> {
    kernel
        .root()
        .peek_live::<Graph>()
        .expect("`graph` is bound")
}

fn agents(kernel: &Kernel) -> Arc<AgentsHandle> {
    kernel
        .root()
        .peek_live::<Agents>()
        .expect("`agents` is bound")
}

/// Boot the shipped `tui` tree with two lanes up.
async fn boot_two() -> (Arc<Kernel>, support::TempDir) {
    boot_real("tui", &[fixture("llm-replay.yml")]).await
}

/// Raw trajectory for an op to have something to be about. Appended directly: what these gates
/// are about is the op, not how the steps got there.
async fn seed(ledger: &LedgerHandle, traj: &str, n: usize) -> Vec<Seq> {
    let mut seqs = Vec::new();
    for i in 0..n {
        let step = ledger
            .0
            .append(Append {
                traj: TrajId::new(traj),
                wake: WakeId::new("wake:seed"),
                kind: StepType::new("thought/text"),
                class: Class::Thought,
                body: serde_json::json!({ "text": format!("seeded thought {i}"), "step_index": i }),
                cites: vec![],
                at: at(i as i64 * 60),
                id: None,
            })
            .await
            .expect("the seed appends");
        seqs.push(step.seq);
    }
    seqs
}

async fn steps_on(ledger: &LedgerHandle, traj: &str) -> Vec<bough_plugin_ledger::Step> {
    ledger
        .0
        .steps(&StepQuery {
            trajs: vec![TrajId::new(traj)],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the chain reads back")
}

/// §4: a bud is a split at a PAST point and THE PARENT NEVER PAUSES. In a booted tree that means
/// three things at once: the parent's chain is unchanged, the parent's agent is still live and
/// takes a message, and the child's branch exists beside it.
#[tokio::test]
async fn a_bud_in_a_booted_tree_leaves_the_parent_running() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_two().await;
    let ledger = ledger(&kernel);
    let seqs = seed(&ledger, "lane/sol", 6).await;
    let before = steps_on(&ledger, "lane/sol").await;
    let past = seqs[2];

    let outcome = graph(&kernel)
        .0
        .apply(&OpRequest::Bud(BudRequest {
            parent: AgentName::new("sol"),
            at_seq: past,
            child: ChildSpec {
                agent: Some(AgentName::new("bud")),
                traj: TrajId::new("lane/bud"),
                routing_refs: BTreeSet::new(),
                wake_classes: BTreeSet::new(),
            },
            reason: "a past thread deserves its own lane".to_string(),
            by: Attribution::Andrey,
            cites: Vec::new(),
            at: at(10_000),
        }))
        .await
        .expect("the bud applies");
    assert_eq!(outcome.kind, OpKind::Bud);

    // 1. The parent's chain is untouched — the past is not partitioned.
    let after = steps_on(&ledger, "lane/sol").await;
    assert_eq!(
        after
            .iter()
            .filter(|s| before.iter().any(|b| b.id == s.id))
            .count(),
        before.len(),
        "every step the parent had before the bud is still on its chain"
    );

    // 2. The parent is still LIVE and still takes work: it never paused.
    let sol = agents(&kernel)
        .by_name(&AgentName::new("sol"))
        .expect("sol is still in the registry after a bud of its own past");
    assert!(
        matches!(
            sol.status(),
            bough_plugin_agents::Status::Idle | bough_plugin_agents::Status::Running
        ),
        "the parent is still a live agent after a bud of its own past"
    );

    // 3. The child exists, beside the parent, with an ancestor edge back to it.
    let edges = ledger
        .0
        .edges(&TrajId::new("lane/bud"))
        .await
        .expect("the edges read");
    assert!(
        edges
            .iter()
            .any(|e| e.kind == EdgeKind::Ancestor && e.parent == TrajId::new("lane/sol")),
        "the bud has no ancestor edge back to the parent: {edges:?}"
    );
    kernel.shutdown().await;
}

/// V7's last row: after a split AND a merge on a booted tree, the kernel's runner reports no
/// violation from `graph-ops`, `ledger` or `agents`.
#[tokio::test]
async fn the_ledger_and_agents_invariants_are_clean_after_a_split_and_a_merge() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_two().await;
    let ledger = ledger(&kernel);
    seed(&ledger, "lane/sol", 8).await;

    let child = |name: &str, r: &str| ChildSpec {
        agent: Some(AgentName::new(name)),
        traj: TrajId::new(format!("lane/{name}")),
        routing_refs: {
            let mut s = BTreeSet::new();
            s.insert(bough_plugin_ledger::Ref::new(r));
            s
        },
        wake_classes: BTreeSet::new(),
    };

    let split = graph(&kernel)
        .0
        .apply(&OpRequest::Split(SplitRequest {
            parent: AgentName::new("sol"),
            at_seq: None,
            children: vec![child("left", "repo:left"), child("right", "repo:right")],
            reason: "two concerns, two lanes".to_string(),
            by: Attribution::Andrey,
            cites: Vec::new(),
            at: at(10_000),
        }))
        .await
        .expect("the split applies");
    assert_eq!(split.kind, OpKind::Split);
    assert_eq!(split.trajs.len(), 2, "a split makes exactly two heads");

    let merge = graph(&kernel)
        .0
        .apply(&OpRequest::Merge(MergeRequest {
            // ANDREY'S CHOICE. The absence of one is a leader question, never a default (§4).
            survivor: AgentName::new("left"),
            absorbed: AgentName::new("right"),
            reason: "they turned out to be one concern".to_string(),
            by: Attribution::Andrey,
            cites: Vec::new(),
            at: at(20_000),
        }))
        .await
        .expect("the merge applies");
    assert_eq!(merge.kind, OpKind::Merge);
    assert_eq!(
        merge
            .rows_deleted
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>(),
        vec!["right".to_string()],
        "the losing ROW is deleted; its trajectory remains"
    );
    assert!(
        !steps_on(&ledger, "lane/right").await.is_empty(),
        "the absorbed lane's trajectory still reads after the merge"
    );

    assert!(kernel.quiesce().await, "the tree quiesces after both ops");
    let violations = kernel.violations();
    assert!(
        violations.is_empty(),
        "a split and a merge on a booted tree left the runner unhappy: {violations:?}"
    );
    kernel.shutdown().await;
}
