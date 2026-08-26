//! V1's third clause, on the SHIPPED tree: each collector is ONE row scheduled through
//! `ctx.schedule`, and disabling the row takes its job off the scheduler with it.
//!
//! This is deliberately not a unit test over `Scheduler::register`: it boots the real
//! `bundles/bough-base.yml` rows through the launcher's own composition path, reads
//! `ctx.schedule`'s live job table, then writes a user patch that disables exactly one collector
//! row and recomposes. Nothing is mocked; the job leaving is the effect's inverse running.

mod support;

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
