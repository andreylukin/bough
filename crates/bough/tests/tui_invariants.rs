//! §0.2's invariant runner over the PHASE-3 tree, and §17 Phase 0's verify item ("the invariant
//! runner reports one planted violation") applied to this phase's specs.
//!
//! The Phase-3 specs were exercised only as pure `check_*` unit functions: nothing proved the
//! kernel had them registered or dispatched at all, which is exactly the bug Phase 2's review
//! found in `tools` (`evaluate_wake_pairing` "was implemented and never registered"). This file
//! boots the SHIPPED tui bundle with `invariants: true` and reads `kernel.violations()` — clean
//! over a real boot, and one planted violation of a Phase-3 spec reported THROUGH the runner.
//!
//! The plants go into each invariant's own recorded stream: these are relations the product code
//! is written not to break, and there is no config that makes a pane render a step twice.

mod support;

use bough_plugin_hello::trace;
use support::{boot_real, TempDir};

async fn boot_tui() -> (std::sync::Arc<bough_kernel::Kernel>, TempDir) {
    boot_real("tui", &[support::fixture("llm-replay.yml")]).await
}

fn violation_names(kernel: &bough_kernel::Kernel) -> Vec<&'static str> {
    kernel
        .violations()
        .into_iter()
        .map(|v| v.invariant)
        .collect()
}

/// The whole Phase-3 tree, booted and collected: nothing this phase added is violated by a boot,
/// and every row whose spec must be collected is live to be collected FROM.
#[tokio::test]
async fn every_phase_three_invariant_reports_clean_over_a_boot() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_tui().await;

    kernel.run_invariants().await;
    assert!(
        kernel.violations().is_empty(),
        "a clean tree must violate nothing: {:#?}",
        kernel.violations()
    );

    // The gate means nothing unless the runner is carrying this phase's specs; naming the rows is
    // what stops a silently-empty spec set from reading as success.
    for row in [
        "commands",
        "tui",
        "tui.strip",
        "tui.focus",
        "tui.search",
        "residents",
        "old-feed",
    ] {
        assert!(
            support::maybe_row(&kernel, row).is_some(),
            "row `{row}` must be live for its invariant to be collected"
        );
    }

    kernel.shutdown().await;
}

/// `tui-focus`: NO STEP IS RENDERED TWICE. Planted at the recorder — a frame whose live tail has
/// diverged from the durable text it is chosen against.
#[tokio::test]
async fn a_planted_focus_frame_that_renders_a_step_twice_is_reported() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_tui().await;

    use bough_plugin_ledger::{StepId, WakeId};
    use bough_plugin_tui_focus::{LiveText, Row};
    let row = |n: u64| Row::Text {
        step: StepId::new(format!("planted-{n}")),
        wake: WakeId::new("w-planted"),
        index: 0,
        text: "the durable text".to_string(),
    };
    bough_plugin_tui_focus::invariant::record_frame(
        &[row(1)],
        &LiveText {
            agent: None,
            // Not prefix-related to the durable text: P3-D12's length rule would render bytes the
            // other half has already shown.
            text: "something else entirely".to_string(),
        },
    );

    kernel.run_invariants().await;
    assert!(
        violation_names(&kernel).contains(&"the_live_tail_and_the_durable_rows_never_overlap"),
        "the runner must report the planted frame: {:#?}",
        kernel.violations()
    );

    bough_plugin_tui_focus::invariant::forget();
    kernel.shutdown().await;
}

/// `tui-search`: every rendered hit names a step the ledger still holds. Planted by recording a
/// hit on a step id nothing ever appended.
#[tokio::test]
async fn a_planted_search_hit_on_a_missing_step_is_reported() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_tui().await;

    use bough_plugin_ledger::{Seq, StepId, StepType, TrajId};
    bough_plugin_tui_search::invariant::record(&[bough_plugin_tui_search::HitRow {
        agent: None,
        traj: TrajId::new("lane/nowhere"),
        step: StepId::new("planted-missing-step"),
        seq: Seq(1),
        kind: StepType::new("thought/text"),
        snippet: "a hit on a step the ledger does not hold".to_string(),
    }]);

    kernel.run_invariants().await;
    assert!(
        violation_names(&kernel).contains(&"every_rendered_hit_names_a_live_step"),
        "the runner must report the planted hit: {:#?}",
        kernel.violations()
    );

    bough_plugin_tui_search::invariant::forget();
    kernel.shutdown().await;
}
