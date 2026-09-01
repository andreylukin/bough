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

use crate::support;

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
/// Wait until the shell has stopped painting: two consecutive frames are the same buffer. The
/// recorders the plant tests write into are LAST-FRAME slots, and a paint that lands after the
/// plant wipes it — including the one extra paint an `Aux` pane costs while it collapses to zero
/// rows after boot (ux-visual D-uxv-1). `Kernel::quiesce` settles fibers, not frames.
async fn settle_frames(kernel: &bough_kernel::Kernel) {
    kernel.quiesce().await;
    let Some(tui) = kernel.root().peek_live::<bough_plugin_tui_shell::Tui>() else {
        return;
    };
    let mut last = tui.last_frame();
    let mut stable = 0;
    for _ in 0..300 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let now = tui.last_frame();
        if std::sync::Arc::ptr_eq(&last, &now) || *last == *now {
            stable += 1;
            if stable >= 5 {
                return;
            }
        } else {
            stable = 0;
            last = now;
        }
    }
}

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

/// Plant, run the invariants, and read the violations back — retrying the plant a few times.
///
/// Both recorders below are LAST-FRAME slots, and `quiesce()` does not promise that the pane has
/// finished its final paint: a boot paint landing after the plant overwrites it and the runner
/// then has nothing to report, which reads as "the spec is not registered". Re-planting is not a
/// weaker assertion — a spec that is genuinely missing reports nothing on every attempt.
async fn reported_after_planting(
    kernel: &bough_kernel::Kernel,
    invariant: &str,
    plant: impl Fn(),
) -> bool {
    for _ in 0..60 {
        // Let every queued paint land BEFORE the plant: on a slow runner the tick that fired
        // during the previous sleep is already in the run queue, and on a current-thread runtime
        // it would otherwise run between the plant and the runner's read — wiping the slot on
        // every attempt, not just the unlucky ones (seen on both CI runners, never locally).
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        plant();
        kernel.run_invariants().await;
        if violation_names(kernel).contains(&invariant) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    false
}

/// `tui-focus`: NO STEP IS RENDERED TWICE. Planted at the recorder — a frame whose live tail has
/// diverged from the durable text it is chosen against.
#[tokio::test]
async fn a_planted_focus_frame_that_renders_a_step_twice_is_reported() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_tui().await;
    // As in the search plant: the recorder is a last-frame slot, so let the boot's own paints
    // finish before planting.
    settle_frames(&kernel).await;

    use bough_plugin_ledger::{StepId, WakeId};
    use bough_plugin_tui_focus::{LiveText, Row};
    let row = |n: u64| Row::Text {
        step: StepId::new(format!("planted-{n}")),
        parts: vec![StepId::new(format!("planted-{n}"))],
        wake: WakeId::new("w-planted"),
        index: 0,
        text: "the durable text".to_string(),
    };
    // Hold the recorder for the whole plant-and-read: the pane repaints whenever the tree
    // stirs, and on a starved runner a paint lands inside every plant-to-read window (three
    // processes, sixty attempts, zero reports on CI). The latch silences the pane's
    // `record_frame`; `plant_frame` below is the latch-immune path this test writes through.
    let _hold = bough_plugin_tui_focus::invariant::hold_for_plant();
    let reported = reported_after_planting(
        &kernel,
        "the_live_tail_and_the_durable_rows_never_overlap",
        || {
            // MERGE (ux1/track B): forget the previous attempt's frame first — the recorder is a
            // LAST-FRAME slot and a tick paint landing between plant and run wipes it, so a retry
            // must start from a clean slot or it re-reads the pane's own frame.
            bough_plugin_tui_focus::invariant::forget();
            bough_plugin_tui_focus::invariant::plant_frame(
                &[row(1)],
                &LiveText {
                    agent: None,
                    // Not prefix-related to the durable text: P3-D12's length rule would render
                    // bytes the other half has already shown.
                    text: "something else entirely".to_string(),
                },
            );
        },
    )
    .await;
    assert!(
        reported,
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
    // The recorder is a LAST-FRAME slot: a boot-time paint that lands after the plant would wipe
    // it and the check would pass vacuously. Let the tree settle first, so the planted frame is
    // the last one.
    settle_frames(&kernel).await;

    use bough_plugin_ledger::{AgentName, StepId};
    let reported = reported_after_planting(&kernel, "every_rendered_hit_names_a_live_step", || {
        // See the focus plant above: clear the last-frame slot before each attempt.
        bough_plugin_tui_search::invariant::forget();
        bough_plugin_tui_search::invariant::record(&[bough_plugin_tui_search::Hit {
            agent: AgentName::new("nowhere"),
            step: StepId::new("planted-missing-step"),
            speaker: "nowhere".to_string(),
            snippet: "a hit on a step the ledger does not hold".to_string(),
            at: 0..1,
        }]);
    })
    .await;
    assert!(
        reported,
        "the runner must report the planted hit: {:#?}",
        kernel.violations()
    );

    bough_plugin_tui_search::invariant::forget();
    kernel.shutdown().await;
}
