//! Phase 4's three governance rows, seen through the LAUNCHER (§8: "these three rows are in
//! `bough-base`"). They are not optional and they are not a terminal feature: they must activate
//! in the default profile AND in `headless`, where no `commands` registry exists for `/seal`,
//! `/reconsolidate`, `/drift` or `/reset` to attach to. And each of them must carry the runtime
//! invariants §0.2 requires, at the only cadence the kernel dispatches.

mod support;

use bough_kernel::{Catalog, FiberState};
use bough_plugin_hello::trace;
use support::{boot_real, fixture, row};

/// The three rows §8 puts in `bough-base`, by entry id and by the plugin each is bound to.
const ROWS: [(&str, &str); 3] = [
    ("rollups", "rollups-summarizer"),
    ("reconsolidation", "reconsolidation"),
    ("drift.watch", "drift-watch"),
];

async fn assert_the_three_rows_are_active(
    profile: &str,
) -> (std::sync::Arc<bough_kernel::Kernel>, support::TempDir) {
    let (kernel, dir) = boot_real(profile, &[fixture("llm-replay.yml")]).await;
    for (id, plugin) in ROWS {
        let r = row(&kernel, id);
        assert_eq!(
            r.state,
            FiberState::Active,
            "row `{id}` must be ACTIVE under `{profile}` (§0.2: an enabled row that never \
             activates is a boot failure)"
        );
        assert_eq!(r.plugin.as_deref(), Some(plugin));
    }
    // The stub is a FIXTURE: in the catalog, in NO bundle (the `ledger-memory` precedent).
    for bundle in ["bough-base.yml", "bough-tui-app.yml", "bough-headless.yml"] {
        let text = std::fs::read_to_string(support::repo_root().join("bundles").join(bundle))
            .expect("the shipped bundle is readable");
        assert!(
            !text.contains("plugin: rollups-none"),
            "`rollups-none` must be in no bundle, but `{bundle}` names it"
        );
    }
    (kernel, dir)
}

#[tokio::test]
async fn the_three_rows_activate_in_the_default_profile() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = assert_the_three_rows_are_active("tui").await;
    kernel.shutdown().await;
}

/// `headless` has no `commands` row, so every command registration in this phase is OPTIONAL
/// (P4-D8): the rows must activate anyway, and the tree must still quiesce with nothing
/// unresolved (asserted by `boot_real` itself).
#[tokio::test]
async fn the_three_rows_activate_headless_without_commands() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = assert_the_three_rows_are_active("headless").await;
    assert!(
        support::maybe_row(&kernel, "commands").is_none(),
        "this test is vacuous if `headless` has a `commands` row"
    );
    kernel.shutdown().await;
}

/// Each governance row declares runtime invariants, at `OnQuiesce` — the only cadence the kernel
/// dispatches (P1-D14) — and a clean boot of the shipped tree reports none of them violated.
#[tokio::test]
async fn every_phase_four_invariant_runs_at_quiesce() {
    let _guard = trace::test_lock();
    let catalog = Catalog::from_inventory().expect("the linked catalog has no duplicate names");
    for (_, plugin) in ROWS {
        let p = catalog
            .get(plugin)
            .unwrap_or_else(|| panic!("`{plugin}` is not in the linked catalog"));
        let specs = p.invariants();
        assert!(
            !specs.is_empty(),
            "`{plugin}` declares no runtime invariant (AGENTS.md requires one or a written reason)"
        );
        for spec in specs {
            assert_eq!(
                spec.cadence,
                bough_kernel::Cadence::OnQuiesce,
                "`{plugin}`'s `{}` would never be dispatched",
                spec.name
            );
        }
    }
    // …and the shipped tree quiesces with none of them reported. `boot_real` turns the runner ON
    // regardless of what the profile says, so this is a real run of every collected spec.
    let (kernel, _dir) = boot_real("tui", &[fixture("llm-replay.yml")]).await;
    // Filter by the SPEC NAMES the four rows collect, not by the row names. A violation carries
    // the name the spec reports under, and the two rollups specs report under the SEAM's name
    // (`rollups`) rather than the provider's — so a row-name filter silently dropped both of
    // them and this assertion was vacuous for half of Phase 4.
    let expected: std::collections::BTreeSet<&'static str> = ROWS
        .iter()
        .filter_map(|(_, plugin)| catalog.get(plugin))
        .flat_map(|p| p.invariants())
        .map(|s| s.name)
        .collect();
    assert!(
        expected.contains("a_range_is_sealed_once_and_generations_never_skip"),
        "the seal-once spec must be among the ones this test watches: {expected:?}"
    );
    let phase_four: Vec<_> = kernel
        .violations()
        .into_iter()
        .filter(|v| expected.contains(v.invariant))
        .collect();
    assert!(
        phase_four.is_empty(),
        "a clean boot reported Phase 4 violations: {phase_four:?}"
    );
    kernel.shutdown().await;
}
