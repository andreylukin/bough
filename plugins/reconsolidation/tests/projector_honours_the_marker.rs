//! V5, end to end: the marker a REAL reconsolidation pass appends is the one a REAL projector
//! honours. The two halves are tested separately elsewhere — the pass against the ledger's row
//! hashes, the projector against a hand-written marker — and that leaves one seam untested: that
//! the bytes the pass writes are the bytes the projector reads. This suite closes it, and it is
//! the only place in the tree where nothing about the marker is written by a fixture.

mod support;

use std::sync::Arc;

use bough_plugin_projection::{AssembleRequest, Projector};
use bough_plugin_projection_assembler::{Assembler, AssemblerConfig};
use support::*;

fn assembler_cfg() -> AssemblerConfig {
    AssemblerConfig {
        budget_tokens: 100_000,
        headroom: 1.0,
        tail_steps: 12,
        tail_floor_steps: 3,
        mail_newest_n: 2,
        max_tiers: 3,
        file_view_dir: std::path::PathBuf::from("/unused-by-this-test"),
    }
}

async fn tail_of(m: &Mounted) -> String {
    let assembler = Assembler::new(Arc::new(assembler_cfg()), m.ledger.clone(), m.ctx.clone());
    let assembled = assembler
        .assemble(&AssembleRequest {
            as_of: None,
            agent: bough_plugin_ledger::AgentName::new(AGENT),
            wake: None,
            at: t0() + chrono::Duration::days(1),
            budget: None,
        })
        .await
        .expect("an answer wake must always be buildable");
    assembled
        .sections
        .iter()
        .find(|s| s.id.as_str() == "tail")
        .map(|s| s.body.clone())
        .unwrap_or_default()
}

#[tokio::test]
async fn a_marker_written_by_a_real_pass_removes_the_step_from_a_real_projection() {
    let m = mount(always_clear(8)).await;
    m.ledger
        .0
        .put_agent(bough_plugin_ledger::AgentRow {
            name: bough_plugin_ledger::AgentName::new(AGENT),
            traj: bough_plugin_ledger::TrajId::new(TRAJ),
            routing_refs: Default::default(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("agents is mutable config");

    let stale = evidence(
        &m.ledger,
        "tool/result",
        &["svc:old"],
        serde_json::json!({ "text": "a stale fact about the old port" }),
        t0() - chrono::Duration::days(400),
    )
    .await;
    let fresh = evidence(
        &m.ledger,
        "tool/result",
        &["svc:new"],
        serde_json::json!({ "text": "a fresh fact about the new port" }),
        t0(),
    )
    .await;

    let before = tail_of(&m).await;
    assert!(
        before.contains("a stale fact about the old port"),
        "the stale step must be IN the projection before the pass, else the test proves nothing:\n{before}"
    );

    let report = m.recon.run(&request(t0())).await.expect("the pass runs");
    assert_eq!(report.expired.len(), 1, "one marker was appended");
    let marker = m
        .ledger
        .0
        .step(&report.expired[0])
        .await
        .expect("a read")
        .expect("the marker the report names is in the ledger");
    let body: bough_plugin_reconsolidation::vocabulary::MemoryExpired =
        serde_json::from_value((*marker.body).clone()).expect("a memory/expired body");
    assert_eq!(
        body.targets,
        vec![bough_plugin_ledger::Ref::new(format!("step:{}", stale.id))],
        "the marker names exactly the stale step"
    );

    let after = tail_of(&m).await;
    assert!(
        !after.contains("a stale fact about the old port"),
        "the projector did not honour the marker the pass wrote:\n{after}"
    );
    assert!(
        after.contains("a fresh fact about the new port"),
        "the projector dropped more than the marker named:\n{after}"
    );
    // …and the raw row is still there: expiry is a projection rule, not a delete (§8).
    assert!(
        m.ledger.0.step(&stale.id).await.expect("a read").is_some(),
        "the raw step was deleted rather than expired"
    );
    assert!(m.ledger.0.step(&fresh.id).await.expect("a read").is_some());
}
