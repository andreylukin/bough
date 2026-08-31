//! V5's leak column (WP-5, decision D-C6): the launcher prints no binding or listener counts, so
//! "nothing leaked" is asserted IN-PROCESS. For every `bough-base` row that the headless profile
//! mounts: boot, record every binding and listener count, disable the row through the launcher's
//! live-recompose path, re-enable it, and require every count back at its pre-disable baseline.
//!
//! This is a stronger statement than two processes compared from the outside: it compares counts
//! across a live disable/re-enable in one address space.
//!
//! HONEST LIMIT (D-C6): `KernelCore` exposes `listener_count(&'static str)` but no enumeration of
//! the registered event names, so the listener half is over a NAMED set of events — the ones the
//! wake flow and the ledger dispatch — and not over "every event in the tree". The binding half
//! has no such limit: `binding_count()` is the whole tree's.

use crate::support;

use bough_kernel::{Kernel, WaterfallEvent};
use bough_plugin_hello::trace;
use support::{boot_real, fixture, maybe_row, TempDir};

/// The events this test watches: EVERY event in the tree, read from `docs/event-catalog.md`.
///
/// MERGE (D-C6 closed): it was a hand-listed five, because `KernelCore` exposes
/// `listener_count(&'static str)` and no enumeration of the registered names — and a hand-list is
/// exactly the shape that goes stale, so a listener leaked on an event nobody thought to name
/// would have passed. `xtask events` DID grow the machine-readable catalog the note hoped for
/// (`make events` is the gate that it matches the tree), so the list comes from there and the
/// honest limit is gone.
///
/// The names are leaked to `&'static str` on purpose: `listener_count` takes one, the set is read
/// once per process, and a test process is short-lived.
fn watched() -> Vec<&'static str> {
    let catalog = std::fs::read_to_string(support::repo_root().join("docs/event-catalog.md"))
        .expect("the generated event catalog");
    let names: Vec<&'static str> = catalog
        .lines()
        .filter_map(|l| l.strip_prefix("| `"))
        .filter_map(|l| l.split('`').next())
        .map(|n| &*Box::leak(n.to_string().into_boxed_str()))
        .collect();
    assert!(
        names.len() >= 40 && names.contains(&bough_plugin_agents::AgentPreStep::NAME),
        "the catalog did not parse into an event list: {names:?}"
    );
    names
}

/// The ids of `bundles/bough-base.yml`, in file order. Read from the SHIPPED bundle, so a row
/// added by a later phase is audited without this test being edited.
fn base_row_ids() -> Vec<String> {
    let yaml = std::fs::read_to_string(support::repo_root().join("bundles/bough-base.yml"))
        .expect("the shipped base bundle");
    yaml.lines()
        .filter_map(|l| l.strip_prefix("- id: "))
        .map(|s| s.trim().to_string())
        .collect()
}

/// Disable `id`, recompose, re-enable it, recompose. The launcher's own live path both ways.
async fn off_and_on(kernel: &Kernel, dir: &TempDir, id: &str) {
    support::write_patch(dir, &format!("entries:\n  {id}:\n    disabled: true\n"));
    support::recompose(kernel, "", dir)
        .await
        .unwrap_or_else(|e| panic!("disabling `{id}` must recompose cleanly: {e}"));
    support::clear_patch(dir);
    support::recompose(kernel, "", dir)
        .await
        .unwrap_or_else(|e| panic!("re-enabling `{id}` must recompose cleanly: {e}"));
}

#[tokio::test]
async fn disabling_a_row_and_re_enabling_it_returns_every_binding_count_to_baseline() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot_real("headless", &[fixture("llm-replay.yml")]).await;
    let core = kernel.core();
    let baseline = core.binding_count();
    let mut audited = 0usize;

    for id in base_row_ids() {
        // A row this profile does not mount is not this test's business.
        if maybe_row(&kernel, &id).is_none() {
            continue;
        }
        off_and_on(&kernel, &dir, &id).await;
        audited += 1;
        assert_eq!(
            core.binding_count(),
            baseline,
            "taking `{id}` down and putting it back changed the tree's binding count"
        );
    }
    // A walk that audited nothing would pass silently, which is the one way this gate can lie.
    assert!(audited > 10, "only {audited} base rows were audited");

    kernel.shutdown().await;
}

#[tokio::test]
async fn disabling_a_row_and_re_enabling_it_returns_every_listener_count_to_baseline() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot_real("headless", &[fixture("llm-replay.yml")]).await;
    let core = kernel.core();
    let baseline: Vec<(&'static str, usize)> = watched()
        .into_iter()
        .map(|e| (e, core.listener_count(e)))
        .collect();
    let mut audited = 0usize;

    for id in base_row_ids() {
        if maybe_row(&kernel, &id).is_none() {
            continue;
        }
        off_and_on(&kernel, &dir, &id).await;
        audited += 1;
        for (event, before) in &baseline {
            assert_eq!(
                core.listener_count(event),
                *before,
                "taking `{id}` down and putting it back changed the listener count on `{event}`"
            );
        }
    }
    assert!(audited > 10, "only {audited} base rows were audited");

    kernel.shutdown().await;
}
