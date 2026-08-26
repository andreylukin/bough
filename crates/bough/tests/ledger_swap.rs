//! The Phase 1 exit gate, ledger half (§17, SWAP). The `ledger` row is switched from
//! `ledger-sqlite` to `ledger-memory` by a live patch edit: the provider row REBUILDS (a `plugin`
//! change is a rebuild, §0.3 line 107), the `projection` row keeps its fiber and re-applies
//! against the new provider, the assembled text is byte-identical, and the retired fiber gives
//! back its binding and its listener. No recompile, no restart, one test process.
//!
//! `$BOUGH_HOME` is process-global, so every test here holds `hello`'s process-wide test lock for
//! its whole body — the same discipline the Phase 0 harness documents.

mod support;

use bough_kernel::FiberState;
use bough_plugin_hello::trace;
use bough_plugin_projection::{AssembleRequest, Projection};
use support::{boot_with, recompose, row, write_patch};

/// The Phase 1 tree: the two product rows exactly as `bundles/bough-base.yml` ships them, plus
/// the fixture row, which is in no bundle.
pub const P1: &str = "\
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

/// The user patch that performs the swap. `config: {}` is `MemoryConfig`'s whole surface.
const SWAP: &str = "\
entries:
  ledger:
    plugin: ledger-memory
    config: {}
";

/// Assemble the probe agent's projection through the LIVE `projection` binding and render it.
async fn assembled_text(kernel: &bough_kernel::Kernel) -> String {
    let projection = kernel
        .root()
        .peek_live::<Projection>()
        .expect("projection is bound");
    let req = AssembleRequest {
        agent: bough_plugin_ledger::AgentName::new("a1"),
        wake: None,
        // Fixed, not `now()`: the assembled text must be a function of (ledger, request, config).
        at: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .into(),
        budget: None,
    };
    projection
        .0
        .assemble(&req)
        .await
        .expect("the projection assembles")
        .to_text()
}

#[tokio::test]
async fn the_base_tree_boots_with_ledger_sqlite() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    let (kernel, _dir) = boot_with(P1).await;

    let ledger = row(&kernel, "ledger");
    assert_eq!(ledger.state, FiberState::Active);
    assert_eq!(ledger.plugin.as_deref(), Some("ledger-sqlite"));
    assert_eq!(row(&kernel, "projection").state, FiberState::Active);
    assert_eq!(row(&kernel, "probe").state, FiberState::Active);

    // The rows in `bundles/bough-base.yml` are the ones this test booted: if the shipped bundle
    // ever stops naming these two plugins, this assertion says so instead of the tree quietly
    // testing a private copy.
    let shipped = std::fs::read_to_string(support::repo_root().join("bundles/bough-base.yml"))
        .expect("the shipped base bundle is readable");
    assert!(shipped.contains("plugin: ledger-sqlite"), "{shipped}");
    assert!(
        shipped.contains("plugin: projection-assembler"),
        "{shipped}"
    );
    assert!(
        !shipped.contains("plugin: projection-probe"),
        "the fixture must be in NO bundle (P1-D16):\n{shipped}"
    );

    // The probe bound against sqlite, declared its two types and scripted its trajectory.
    assert!(bough_plugin_projection_probe::saw("ledger=ledger-sqlite"));
    assert!(bough_plugin_projection_probe::saw("step-types"));
    assert!(bough_plugin_projection_probe::saw("scripted"));
    assert!(bough_plugin_projection_probe::saw("sections"));

    kernel.shutdown().await;
}

#[tokio::test]
async fn a_patch_swaps_the_row_to_ledger_memory_without_a_recompile() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    let (kernel, dir) = boot_with(P1).await;
    let ledger_before = row(&kernel, "ledger").uid.expect("uid");
    let fingerprint_before = kernel.snapshot().fingerprint;

    write_patch(&dir, SWAP);
    recompose(&kernel, P1, &dir)
        .await
        .expect("the swap composes");

    let ledger = row(&kernel, "ledger");
    assert_eq!(ledger.state, FiberState::Active);
    assert_eq!(ledger.plugin.as_deref(), Some("ledger-memory"));
    assert_ne!(
        ledger.uid.expect("uid"),
        ledger_before,
        "a `plugin` change REBUILDS the row (§0.3 line 107), so the uid must move"
    );
    assert_ne!(kernel.snapshot().fingerprint, fingerprint_before);

    kernel.shutdown().await;
}

#[tokio::test]
async fn the_assembler_reloads_against_the_new_provider() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    let (kernel, dir) = boot_with(P1).await;
    let projection_before = row(&kernel, "projection").uid.expect("uid");
    bough_plugin_projection_probe::clear();

    write_patch(&dir, SWAP);
    recompose(&kernel, P1, &dir)
        .await
        .expect("the swap composes");

    assert_eq!(
        row(&kernel, "projection").uid.expect("uid"),
        projection_before,
        "the assembler's own row did not change; it RELOADS in the same fiber"
    );
    // The reload really happened, and it bound against the memory provider.
    let t = bough_plugin_projection_probe::trace();
    assert!(
        bough_plugin_projection_probe::saw("ledger=ledger-memory"),
        "the probe never re-applied against the new provider: {t:?}"
    );
    assert_eq!(
        kernel
            .root()
            .peek_live::<Projection>()
            .expect("projection is bound")
            .0
            .provider(),
        "projection-assembler"
    );

    kernel.shutdown().await;
}

#[tokio::test]
async fn the_golden_suite_passes_against_the_swapped_provider() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    let (kernel, dir) = boot_with(P1).await;
    let before = assembled_text(&kernel).await;
    assert!(
        !before.trim().is_empty(),
        "an empty projection would make the equality below vacuous"
    );

    write_patch(&dir, SWAP);
    recompose(&kernel, P1, &dir)
        .await
        .expect("the swap composes");
    let after = assembled_text(&kernel).await;

    assert_eq!(
        before, after,
        "the assembled projection must be byte-identical across the provider swap"
    );

    kernel.shutdown().await;
}

#[tokio::test]
async fn the_retired_provider_leaves_no_binding_and_no_listener() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    let (kernel, dir) = boot_with(P1).await;
    let sqlite_uid = row(&kernel, "ledger").uid.expect("uid");
    let bindings_before = kernel.core().binding_count();
    let listeners_before = kernel.core().listener_count("ledger/step");
    assert!(
        bindings_before > 0 && listeners_before > 0,
        "the fixture must hold at least one binding and one ledger/step listener, or the \
         equalities below are vacuous: bindings={bindings_before} listeners={listeners_before}"
    );

    write_patch(&dir, SWAP);
    recompose(&kernel, P1, &dir)
        .await
        .expect("the swap composes");

    assert_eq!(
        kernel.core().binding_count(),
        bindings_before,
        "the retired ledger provider left a service binding behind"
    );
    assert_eq!(
        kernel.core().listener_count("ledger/step"),
        listeners_before,
        "the retired ledger provider left a ledger/step listener behind"
    );

    let snapshot = kernel.snapshot();
    let providers: Vec<_> = snapshot
        .rows
        .iter()
        .filter(|r| r.provides.contains(&"ledger"))
        .collect();
    assert_eq!(providers.len(), 1, "exactly one live ledger binding");
    assert_ne!(providers[0].uid.expect("uid"), sqlite_uid);
    assert_eq!(
        kernel
            .root()
            .peek_live::<bough_plugin_ledger::Ledger>()
            .expect("ledger is bound")
            .0
            .provider(),
        "ledger-memory"
    );

    kernel.shutdown().await;
}

/// §0.2, "unload leaves no trace": a store handle that outlives its row (the assembler captures
/// one for its whole life) must not keep writing through a retired Context whose `ledger/step`
/// listener is already disposed.
#[tokio::test]
async fn a_handle_that_outlives_its_row_refuses_to_write() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    let (kernel, dir) = boot_with(P1).await;
    // A clone kept deliberately, exactly as a consumer would hold it.
    let retained = kernel
        .root()
        .peek_live::<bough_plugin_ledger::Ledger>()
        .expect("ledger is bound")
        .as_ref()
        .clone();
    retained
        .0
        .append(bough_plugin_ledger::Append {
            traj: bough_plugin_ledger::TrajId::new("t-retire"),
            wake: bough_plugin_ledger::WakeId::new("w1"),
            kind: bough_plugin_ledger::StepType::new("wake/start"),
            class: bough_plugin_ledger::Class::Thought,
            body: serde_json::json!({ "urgency": "immediate" }),
            cites: Vec::new(),
            at: chrono::Utc::now(),
            id: None,
        })
        .await
        .expect("the live row accepts an append");

    write_patch(&dir, SWAP);
    recompose(&kernel, P1, &dir)
        .await
        .expect("the swap composes");

    let err = retained
        .0
        .append(bough_plugin_ledger::Append {
            traj: bough_plugin_ledger::TrajId::new("t-retire"),
            wake: bough_plugin_ledger::WakeId::new("w1"),
            kind: bough_plugin_ledger::StepType::new("wake/start"),
            class: bough_plugin_ledger::Class::Thought,
            body: serde_json::json!({ "urgency": "immediate" }),
            cites: Vec::new(),
            at: chrono::Utc::now(),
            id: None,
        })
        .await
        .expect_err("a retired store must refuse, not write unobserved");
    assert!(
        err.to_string().contains("retired"),
        "the refusal must say why: {err}"
    );

    kernel.shutdown().await;
}
