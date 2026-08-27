//! The Phase 1 exit gate, projection half (§17, SWAP). Disabling the `projection-assembler` row by
//! patch leaves every consumer of `ctx.projection` PENDING with nothing FAILED and the `ledger`
//! row untouched; removing the `disabled` line restores every consumer and its sections. No
//! recompile, no restart.

use crate::support;

use bough_kernel::FiberState;
use bough_plugin_hello::trace;
use bough_plugin_projection::Projection;
use support::{boot_with, clear_patch, recompose, row, write_patch};

/// The Phase 1 tree, shared with `ledger_swap.rs` by value rather than by a support module: the
/// Phase 0 harness file is not this work package's to edit.
const P1: &str = "\
- id: ledger
  plugin: ledger-sqlite
  config:
    path: !!expr 'bough_path(\"ledger.db\")'
    busy_timeout_ms: 5000
- id: projection
  plugin: projection-assembler
  config:
    budget_tokens: 160000
    headroom: 0.6
    tail_steps: 60
    tail_floor_steps: 10
    mail_newest_n: 5
    max_tiers: 3
    file_view_dir: !!expr 'bough_path(\"views\")'
- id: probe
  plugin: projection-probe
  config:
    traj: t1
    agent: a1
    steps: 3
";

const DISABLE: &str = "\
entries:
  projection:
    disabled: true
";

#[tokio::test]
async fn disabling_the_assembler_leaves_consumers_pending() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    let (kernel, dir) = boot_with(P1).await;
    assert_eq!(row(&kernel, "probe").state, FiberState::Active);

    write_patch(&dir, DISABLE);
    recompose(&kernel, P1, &dir)
        .await
        .expect("disabling a row composes");

    let projection = row(&kernel, "projection");
    assert_eq!(projection.state, FiberState::Inactive);
    assert!(projection.disabled);

    let probe = row(&kernel, "probe");
    assert_eq!(probe.state, FiberState::Pending);
    assert_eq!(probe.unmet, vec!["projection".to_string()]);

    kernel.shutdown().await;
}

#[tokio::test]
async fn disabling_the_assembler_fails_nothing() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    let (kernel, dir) = boot_with(P1).await;
    let ledger_before = row(&kernel, "ledger").uid.expect("uid");

    write_patch(&dir, DISABLE);
    recompose(&kernel, P1, &dir)
        .await
        .expect("disabling a row composes");

    fn failed(rows: &[bough_kernel::RowSnapshot], out: &mut Vec<String>) {
        for r in rows {
            if r.state == FiberState::Failed {
                out.push(r.id.as_str().to_string());
            }
            failed(&r.children, out);
        }
    }
    let mut bad = Vec::new();
    failed(&kernel.snapshot().rows, &mut bad);
    assert!(bad.is_empty(), "rows FAILED on a clean disable: {bad:?}");

    // The ledger is a bystander: its fiber must not move.
    let ledger = row(&kernel, "ledger");
    assert_eq!(ledger.state, FiberState::Active);
    assert_eq!(ledger.uid.expect("uid"), ledger_before);

    kernel.shutdown().await;
}

#[tokio::test]
async fn re_enabling_the_assembler_restores_every_consumer() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    let (kernel, dir) = boot_with(P1).await;

    write_patch(&dir, DISABLE);
    recompose(&kernel, P1, &dir)
        .await
        .expect("disabling a row composes");
    assert_eq!(row(&kernel, "probe").state, FiberState::Pending);
    bough_plugin_projection_probe::clear();

    clear_patch(&dir);
    recompose(&kernel, P1, &dir)
        .await
        .expect("re-enabling composes");

    assert_eq!(row(&kernel, "projection").state, FiberState::Active);
    let probe = row(&kernel, "probe");
    assert_eq!(probe.state, FiberState::Active);
    assert!(probe.unmet.is_empty());
    assert!(
        bough_plugin_projection_probe::saw("sections"),
        "the restored probe must have re-registered its sections: {:?}",
        bough_plugin_projection_probe::trace()
    );

    kernel.shutdown().await;
}

#[tokio::test]
async fn the_probes_sections_are_gone_while_the_assembler_is_disabled() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    let (kernel, dir) = boot_with(P1).await;
    assert!(
        kernel.root().peek_live::<Projection>().is_some(),
        "the projection key must be bound before it is taken away"
    );

    write_patch(&dir, DISABLE);
    recompose(&kernel, P1, &dir)
        .await
        .expect("disabling a row composes");

    // The whole seam is gone: no binding at all, so there is nothing left holding the probe's two
    // sections. That is stronger than "the text no longer contains them" and needs no assembler.
    assert!(
        kernel.root().peek_live::<Projection>().is_none(),
        "the retired assembler left the `projection` key bound"
    );
    let snapshot = kernel.snapshot();
    assert_eq!(
        snapshot
            .rows
            .iter()
            .filter(|r| r.provides.contains(&"projection"))
            .count(),
        0
    );

    // And when it comes back, the sections come back with it and render again.
    clear_patch(&dir);
    recompose(&kernel, P1, &dir)
        .await
        .expect("re-enabling composes");
    let handle = kernel
        .root()
        .peek_live::<Projection>()
        .expect("projection is bound again");
    let text = handle
        .0
        .assemble(&bough_plugin_projection::AssembleRequest {
            as_of: None,
            agent: bough_plugin_ledger::AgentName::new("a1"),
            wake: None,
            at: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .into(),
            budget: None,
        })
        .await
        .expect("the projection assembles")
        .to_text();
    assert!(
        text.contains("probe (agent)"),
        "the probe's agent-scoped section must shadow its global one and appear:\n{text}"
    );
    assert!(
        !text.contains("probe (global)"),
        "the global section must be SHADOWED for this agent:\n{text}"
    );

    kernel.shutdown().await;
}
