//! The Phase 3 exit gate, SWAP half (§17 Phase 3): the `tui.search` pane row is disabled by a
//! live patch edit while the tree is up. Its pane leaves the layout, the remaining panes reflow,
//! re-enabling it returns the pane, and the retired row gives back its pane, its listeners and
//! its bindings. No recompile, no restart, one test process — through the LAUNCHER'S OWN
//! recompose (`bough::watch::recompose_once`), the `ledger_swap.rs` precedent.
//!
//! The shell runs on the HEADLESS backend here (P3-D2: no TTY means `TestBackend`, not a boot
//! failure), which is what lets this be an ordinary `cargo test`.

mod support;

use bough_kernel::FiberState;
use bough_plugin_hello::trace;
use bough_plugin_tui_shell::Tui;
use support::{boot_real, maybe_row, recompose, row, write_patch, TempDir};

/// Disable the search pane row. `disabled: true` is the whole patch: §17's swap is a CONFIG edit.
const DISABLE_SEARCH: &str = "\
entries:
  tui.search:
    disabled: true
";

/// The five rows this phase's bundle adds on top of `bough-base` that must ALL be active.
const FIVE: [&str; 5] = ["commands", "tui", "tui.strip", "tui.focus", "tui.search"];

/// The live `tui` handle's pane ids, sorted the way `panes()` sorts them.
fn pane_ids(kernel: &bough_kernel::Kernel) -> Vec<String> {
    kernel
        .root()
        .peek_live::<Tui>()
        .expect("`tui` is bound")
        .panes()
        .into_iter()
        .map(|p| p.id.to_string())
        .collect()
}

/// Boot the SHIPPED `profiles/tui.yml` + `bundles/`, with no live model anywhere: the
/// `llm.anthropic` row is swapped for `llm-replay` by the fixture patch, exactly as Phase 2's
/// gates do it.
async fn boot_tui() -> (std::sync::Arc<bough_kernel::Kernel>, TempDir) {
    boot_real("tui", &[support::fixture("llm-replay.yml")]).await
}

#[tokio::test]
async fn the_tui_bundle_boots_with_all_five_rows_active() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_tui().await;

    for id in FIVE {
        assert_eq!(
            row(&kernel, id).state,
            FiberState::Active,
            "row `{id}` must be active"
        );
    }
    // The two rows this phase adds that are NOT terminal features, and are in the same bundle.
    assert_eq!(row(&kernel, "residents").state, FiberState::Active);
    assert_eq!(row(&kernel, "old-feed").state, FiberState::Active);

    // The fixture rows must be in NO bundle (the `projection-probe` precedent, P1-D16).
    let shipped = std::fs::read_to_string(support::repo_root().join("bundles/bough-tui-app.yml"))
        .expect("the shipped tui bundle is readable");
    assert!(!shipped.contains("plugin: tui-probe"), "{shipped}");
    assert!(!shipped.contains("plugin: tui-never"), "{shipped}");

    let panes = pane_ids(&kernel);
    for want in ["tui.strip", "tui.focus", "tui.search"] {
        assert!(
            panes.iter().any(|p| p == want),
            "no `{want}` pane: {panes:?}"
        );
    }

    kernel.shutdown().await;
}

#[tokio::test]
async fn disabling_the_search_row_removes_its_pane_and_reflows() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot_tui().await;
    let before = pane_ids(&kernel);
    assert!(before.iter().any(|p| p == "tui.search"), "{before:?}");
    let tui = kernel.root().peek_live::<Tui>().expect("`tui` is bound");
    let search_before = tui
        .rect_of(&bough_plugin_tui_shell::pane::PaneId::new("tui.search"))
        .expect("the search pane has a rectangle before the patch");
    let focus_before = tui
        .rect_of(&bough_plugin_tui_shell::pane::PaneId::new("tui.focus"))
        .expect("the focus pane has a rectangle");
    assert!(search_before.height > 0);

    write_patch(&dir, DISABLE_SEARCH);
    recompose(&kernel, "", &dir)
        .await
        .expect("the swap composes");

    let after = pane_ids(&kernel);
    assert!(
        !after.iter().any(|p| p == "tui.search"),
        "the pane must be gone: {after:?}"
    );
    assert!(
        after.iter().any(|p| p == "tui.focus") && after.iter().any(|p| p == "tui.strip"),
        "the other panes stay: {after:?}"
    );
    assert_eq!(
        after.len(),
        before.len() - 1,
        "exactly one pane left the layout"
    );

    // …AND REFLOWS — the second half of this test's own name, on geometry rather than membership.
    assert_eq!(
        tui.rect_of(&bough_plugin_tui_shell::pane::PaneId::new("tui.search")),
        None,
        "a retired pane has no rectangle"
    );
    // `rect_of` reads the LAST PAINTED frame's rectangles: the disposer takes the retired pane out
    // of the layout immediately, but the remaining panes only grow on the next paint. Ask for one
    // and wait for it, rather than reading a frame from before the swap.
    let focus_id = bough_plugin_tui_shell::pane::PaneId::new("tui.focus");
    let mut focus_after = focus_before;
    for _ in 0..200 {
        tui.redraw();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if let Some(r) = tui.rect_of(&focus_id) {
            focus_after = r;
            if r.height > focus_before.height {
                break;
            }
        }
    }
    assert!(
        focus_after.height > focus_before.height,
        "the freed rows go to the remaining panes: {focus_before:?} -> {focus_after:?}"
    );

    kernel.shutdown().await;
}

#[tokio::test]
async fn re_enabling_the_search_row_returns_the_pane() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot_tui().await;
    let before = pane_ids(&kernel);

    write_patch(&dir, DISABLE_SEARCH);
    recompose(&kernel, "", &dir)
        .await
        .expect("the swap composes");
    assert!(!pane_ids(&kernel).iter().any(|p| p == "tui.search"));

    support::clear_patch(&dir);
    recompose(&kernel, "", &dir)
        .await
        .expect("removing the patch composes");

    assert_eq!(
        pane_ids(&kernel),
        before,
        "the layout returns to exactly what it was"
    );
    assert_eq!(row(&kernel, "tui.search").state, FiberState::Active);

    kernel.shutdown().await;
}

#[tokio::test]
async fn the_retired_search_row_leaves_no_pane_no_listener_and_no_binding() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot_tui().await;
    let shell_uid = row(&kernel, "tui").uid.expect("uid");

    write_patch(&dir, DISABLE_SEARCH);
    recompose(&kernel, "", &dir)
        .await
        .expect("the swap composes");

    // The row is gone or inactive; either way it holds nothing.
    match maybe_row(&kernel, "tui.search") {
        None => {}
        Some(r) => assert_ne!(
            r.state,
            FiberState::Active,
            "a disabled row must not stay active"
        ),
    }
    assert!(!pane_ids(&kernel).iter().any(|p| p == "tui.search"));

    // Registrations are EFFECTS: unloading the row leaves no trace in the shell it registered
    // into, and the shell itself never rebuilt.
    assert_eq!(
        row(&kernel, "tui").uid.expect("uid"),
        shell_uid,
        "the shell keeps its fiber: only the pane row changed"
    );
    let snapshot = kernel.snapshot();
    let listeners: Vec<_> = snapshot
        .rows
        .iter()
        .flat_map(|r| r.realms.iter())
        .filter(|(_, v)| format!("{v:?}").contains("tui.search"))
        .collect();
    assert!(
        listeners.is_empty(),
        "the retired row left something behind: {listeners:?}"
    );

    kernel.shutdown().await;
}
