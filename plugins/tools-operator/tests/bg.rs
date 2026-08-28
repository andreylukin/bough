//! `bg` on real processes: the three ops round-trip, `bg_max` bounds LIVE jobs, a killed job's log
//! survives its process, and disposing the registry leaves no orphan still writing.

use std::path::PathBuf;
use std::sync::Arc;

use bough_plugin_tools_operator::bg::{BgId, BgJobs};
use bough_plugin_tools_operator::OperatorConfig;
use tempfile::TempDir;

fn cfg(dir: &TempDir, bg_max: usize) -> Arc<OperatorConfig> {
    Arc::new(OperatorConfig {
        max_view_bytes: 1_000_000,
        max_files_per_patch: 8,
        bg_log_dir: dir.path().join("logs"),
        bg_max,
        bg_poll_ms: 20,
        ledger_page: 50,
        schedule_max_horizon_days: 30,
        schedule_tick_ms: 1_000,
    })
}

fn jobs(dir: &TempDir, bg_max: usize) -> Arc<BgJobs> {
    BgJobs::new(cfg(dir, bg_max), dir.path().to_path_buf())
}

/// Poll until `f` holds or the budget runs out. Nothing here sleeps a fixed amount of time and
/// then asserts: that is how a background-job test becomes flaky on a loaded machine.
async fn until(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..300 {
        if f() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    false
}

#[tokio::test]
async fn start_output_and_kill_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = jobs(&dir, 4);
    let job = jobs
        .start("greet", "echo hello-from-bg")
        .expect("it starts");
    assert!(job.pid.is_some(), "a started job has a pid");
    assert_eq!(job.exit, None, "a just-started job has not exited");

    let id = job.id.clone();
    let done = until(|| {
        jobs.output(&id)
            .map(|(j, _)| j.exit.is_some())
            .unwrap_or(false)
    })
    .await;
    assert!(done, "the job never exited");
    let (job, text) = jobs.output(&id).unwrap();
    assert_eq!(job.exit, Some(0), "`echo` exits 0");
    assert!(
        text.contains("hello-from-bg"),
        "the log holds the output: {text:?}"
    );
    assert_eq!(jobs.live(), 0, "an exited job is not live");

    // `kill` on an already-exited job is the status, not an error.
    let job = jobs
        .kill(&id)
        .expect("killing a finished job is not an error");
    assert_eq!(job.exit, Some(0));
    let missing = jobs
        .kill(&BgId::new("nope"))
        .expect_err("an unknown id is NotFound");
    assert_eq!(missing.kind, bough_plugin_tools::FailureClass::NotFound);
}

#[tokio::test]
async fn bg_max_refuses_the_next_one() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = jobs(&dir, 2);
    let a = jobs.start("a", "sleep 30").expect("first");
    let _b = jobs.start("b", "sleep 30").expect("second");
    let refused = jobs
        .start("c", "sleep 30")
        .expect_err("the third is refused");
    assert_eq!(refused.kind, bough_plugin_tools::FailureClass::Blocked);
    assert!(refused.message.contains("bg_max"), "{}", refused.message);

    // The bound is on LIVE jobs: killing one makes room again.
    jobs.kill(&a.id).unwrap();
    jobs.start("c", "sleep 30").expect("room was freed");
    jobs.kill_all();
}

#[tokio::test]
async fn a_killed_jobs_log_is_still_readable() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = jobs(&dir, 4);
    let job = jobs
        .start("chatty", "echo before-the-kill; sleep 30")
        .expect("it starts");
    let id = job.id.clone();
    let wrote = until(|| {
        jobs.output(&id)
            .map(|(_, t)| t.contains("before-the-kill"))
            .unwrap_or(false)
    })
    .await;
    assert!(wrote, "the job never wrote its first line");

    jobs.kill(&id).unwrap();
    let (job, text) = jobs.output(&id).expect("a killed job is still readable");
    assert!(job.exit.is_some(), "a killed job reports an exit");
    assert!(
        text.contains("before-the-kill"),
        "the log outlives the process: {text:?}"
    );
    assert!(job.log.exists(), "the log file is still on disk");
}

#[tokio::test]
async fn disposal_kills_every_live_job() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = jobs(&dir, 4);
    // A job that keeps writing: if the process survives disposal, the log keeps growing, and that
    // is a far more honest orphan check than `kill -0` (which a zombie also answers).
    for n in 0..3 {
        jobs.start(
            &format!("loop{n}"),
            "while true; do echo tick; sleep 0.05; done",
        )
        .expect("it starts");
    }
    let first = jobs.all()[0].id.clone();
    let growing = until(|| {
        jobs.output(&first)
            .map(|(_, t)| t.len() > 20)
            .unwrap_or(false)
    })
    .await;
    assert!(growing, "the job never started producing output");

    jobs.kill_all();
    assert_eq!(jobs.live(), 0, "no job is live after disposal");

    let sizes: Vec<usize> = jobs
        .all()
        .iter()
        .map(|j| {
            std::fs::metadata(&j.log)
                .map(|m| m.len() as usize)
                .unwrap_or(0)
        })
        .collect();
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let after: Vec<usize> = jobs
        .all()
        .iter()
        .map(|j| {
            std::fs::metadata(&j.log)
                .map(|m| m.len() as usize)
                .unwrap_or(0)
        })
        .collect();
    assert_eq!(
        sizes, after,
        "a log that keeps growing is an orphan process"
    );
}

#[tokio::test]
async fn the_log_lives_under_bg_log_dir() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = jobs(&dir, 2);
    let job = jobs.start("x", "true").unwrap();
    assert_eq!(
        job.log.parent().map(PathBuf::from),
        Some(dir.path().join("logs")),
        "the log lives where the config says"
    );
    assert!(job
        .log
        .file_name()
        .unwrap()
        .to_string_lossy()
        .ends_with(".log"));
}
