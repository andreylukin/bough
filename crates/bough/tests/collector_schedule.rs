//! V1's third clause, on the SHIPPED tree: each collector is ONE row scheduled through
//! `ctx.schedule`, and disabling the row takes its job off the scheduler with it.
//!
//! This is deliberately not a unit test over `Scheduler::register`: it boots the real
//! `bundles/bough-base.yml` rows through the launcher's own composition path, reads
//! `ctx.schedule`'s live job table, then writes a user patch that disables exactly one collector
//! row and recomposes. Nothing is mocked; the job leaving is the effect's inverse running.

use crate::support;

use bough_plugin_hello::trace;
use bough_plugin_schedule::Schedule;
use support::{boot_real, maybe_row, recompose, row, row_ctx, write_patch};

/// Disable the GitHub collector row only.
const DISABLE_GITHUB: &str = "\
entries:
  collect.github:
    disabled: true
";

fn job_names(kernel: &std::sync::Arc<bough_kernel::Kernel>, from_row: &str) -> Vec<String> {
    let schedule = row_ctx(kernel, from_row)
        .get::<Schedule>()
        .expect("the schedule key is bound");
    schedule
        .0
        .jobs()
        .iter()
        .map(|j| j.name.to_string())
        .collect()
}

#[tokio::test]
async fn disabling_the_row_removes_its_job_from_schedule_jobs() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot_real("headless", &[]).await;

    // Both collectors mounted, and each registered exactly one job, owned by its own row.
    assert_eq!(
        row(&kernel, "collect.github").state,
        bough_kernel::FiberState::Active
    );
    let before = job_names(&kernel, "collect.linear");
    assert!(
        before.contains(&"collector-github".to_string()),
        "{before:?}"
    );
    assert!(
        before.contains(&"collector-linear".to_string()),
        "{before:?}"
    );
    let gh_jobs: Vec<_> = row_ctx(&kernel, "collect.linear")
        .get::<Schedule>()
        .unwrap()
        .0
        .jobs()
        .into_iter()
        .filter(|j| j.name.as_str() == "collector-github")
        .collect();
    assert_eq!(gh_jobs.len(), 1, "one row, one job: {gh_jobs:?}");
    assert_eq!(gh_jobs[0].owner.as_str(), "collect.github");

    write_patch(&dir, DISABLE_GITHUB);
    recompose(&kernel, "", &dir)
        .await
        .expect("the patch composes");

    // The row is gone (or inactive), and its job left with it — while the sibling collector's
    // job is untouched.
    let after_row = maybe_row(&kernel, "collect.github");
    assert!(
        after_row
            .as_ref()
            .map(|r| r.state != bough_kernel::FiberState::Active)
            .unwrap_or(true),
        "{after_row:?}"
    );
    let after = job_names(&kernel, "collect.linear");
    assert!(
        !after.contains(&"collector-github".to_string()),
        "{after:?}"
    );
    assert!(after.contains(&"collector-linear".to_string()), "{after:?}");

    kernel.shutdown().await;
}

/// MERGE (track B → Phase 5): the collectors route through `mail-router` IN THE SHIPPED TREE.
///
/// `bundles/bough-base.yml` ships both collector rows with `deliver_to: []`, because `mail` is in
/// the same bundle and the router is what chooses recipients from the refs a collector cites. That
/// only holds if the row's COMMITTED VIEW actually carries `mail` — an optional key absent at
/// activation stays absent for that row's whole life (§0.3), so a collector that activated in the
/// window before `mail` was bound would fall back to an EMPTY `deliver_to` and drop every item it
/// swept, warning once at boot and never again.
///
/// It used to. `bough --check` printed "no `mail` seam and an empty `deliver_to`" on most boots,
/// and `phase6_swap.rs` passed either way because its layer sets `deliver_to: [sol]`.
#[tokio::test]
async fn both_collectors_have_the_mail_seam_in_the_shipped_tree() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_real("headless", &[]).await;

    for id in ["collect.github", "collect.linear"] {
        assert_eq!(row(&kernel, id).state, bough_kernel::FiberState::Active);
        assert!(
            row_ctx(&kernel, id)
                .get::<bough_plugin_mail_router::Mail>()
                .is_ok(),
            "`{id}` activated without the `mail` seam, so it would deliver into an empty \
             `deliver_to` and drop everything it sweeps"
        );
    }
    kernel.shutdown().await;
}
