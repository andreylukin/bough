//! V13's second clause, on the SHIPPED tree: the two system passes are schedule rows that register
//! their jobs through `ctx.schedule`, and the reconsolidation job reports PENDING — never FAILED —
//! while `/reconsolidate` does not exist on this branch.
//!
//! Nothing is mocked: this boots `bundles/bough-base.yml` through the launcher's own composition
//! path, reads the live job table off `ctx.schedule`, and fires the reconsolidation job through the
//! real scheduler.

use crate::support;

use bough_plugin_hello::trace;
use bough_plugin_schedule::{JobName, JobOutcome, Schedule};
use support::{boot_real, row, row_ctx};

#[tokio::test]
async fn both_system_rows_register_their_jobs_on_ctx_schedule() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_real("headless", &[]).await;

    assert_eq!(
        row(&kernel, "schedule.catch-up").state,
        bough_kernel::FiberState::Active
    );
    assert_eq!(
        row(&kernel, "schedule.reconsolidate").state,
        bough_kernel::FiberState::Active
    );

    let jobs = row_ctx(&kernel, "schedule.catch-up")
        .get::<Schedule>()
        .expect("the schedule key is bound")
        .0
        .jobs();

    let catch_up: Vec<_> = jobs
        .iter()
        .filter(|j| j.name.as_str() == "system:catch-up")
        .collect();
    assert_eq!(catch_up.len(), 1, "one row, one job: {jobs:?}");
    assert_eq!(catch_up[0].owner.as_str(), "schedule.catch-up");

    let recon: Vec<_> = jobs
        .iter()
        .filter(|j| j.name.as_str() == "system:reconsolidate")
        .collect();
    assert_eq!(recon.len(), 1, "one row, one job: {jobs:?}");
    assert_eq!(recon[0].owner.as_str(), "schedule.reconsolidate");

    kernel.shutdown().await;
}

#[tokio::test]
async fn the_reconsolidate_job_is_pending_not_failed_and_the_row_survives_three_fires() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_real("headless", &[]).await;

    let schedule = row_ctx(&kernel, "schedule.reconsolidate")
        .get::<Schedule>()
        .expect("the schedule key is bound");

    for i in 0..3 {
        let run = schedule
            .0
            .fire_now(&JobName::new("system:reconsolidate"))
            .await
            .expect("the job is registered and fires");
        match &run.outcome {
            JobOutcome::Pending { reason } => {
                // The pending chain's exact link moved when `commands` joined `bough-base`
                // (2026-08-31): with the seam and the command both present at boot, the missing
                // referent on a bare headless home is the AGENT. Any of the three is the same
                // property: pending names the missing referent, and is never Failed.
                assert!(
                    reason.contains("no command named `reconsolidate`")
                        || reason.contains("commands seam")
                        || reason.contains("no live agent"),
                    "fire {i}: PENDING must name the missing referent, got: {reason}"
                );
            }
            other => panic!("fire {i}: expected Pending, got {other:?}"),
        }
        // The row does not fail out from under a PENDING outcome (P6-D2).
        assert_eq!(
            row(&kernel, "schedule.reconsolidate").state,
            bough_kernel::FiberState::Active,
            "fire {i}"
        );
        assert_eq!(
            schedule
                .0
                .jobs()
                .iter()
                .filter(|j| j.name.as_str() == "system:reconsolidate")
                .count(),
            1,
            "fire {i}: the job stays registered"
        );
    }

    kernel.shutdown().await;
}
