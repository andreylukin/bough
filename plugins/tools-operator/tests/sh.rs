//! `sh` on real processes: the legs run CONCURRENTLY, a non-zero exit is data rather than a
//! failure, the results come back IN LEG ORDER, and an untagged leg is refused before anything
//! runs.
//!
//! MERGE: this tool did not exist. `plugins/tools-codemode/src/surface/shell.md` had taught
//! `sh([{cmd, tags}, …])` since the phase was written while no row registered it, so the sandbox
//! advertised a function the model could not call (`docs/codemode-merge-notes.md` §9).

use std::path::PathBuf;
use std::sync::Arc;

use bough_plugin_tools::FailureClass;
use bough_plugin_tools_operator::sh::{leg_tags, legs_of, Leg, Sh};
use bough_plugin_tools_operator::OperatorConfig;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn cfg() -> Arc<OperatorConfig> {
    Arc::new(OperatorConfig {
        max_view_bytes: 1_000_000,
        max_files_per_patch: 8,
        bg_log_dir: PathBuf::from("/tmp"),
        bg_max: 4,
        bg_poll_ms: 20,
        ledger_page: 50,
        schedule_max_horizon_days: 30,
        schedule_tick_ms: 1_000,
        sh_max_legs: 4,
        sh_timeout_ms: 5_000,
        sh_tags_min: 3,
        sh_tags_max: 5,
    })
}

fn sh(dir: &TempDir) -> Sh {
    Sh {
        cfg: cfg(),
        root: dir.path().to_path_buf(),
    }
}

fn leg(cmd: &str, tags: serde_json::Value) -> Leg {
    serde_json::from_value(serde_json::json!({ "cmd": cmd, "tags": tags })).unwrap()
}

#[test]
fn a_leg_list_parses_in_both_the_object_and_the_bare_array_spelling() {
    let object = serde_json::json!({"legs": [{"cmd": "true", "tags": ["a", "b", "c"]}]});
    let bare = serde_json::json!([{"cmd": "true", "tags": ["a", "b", "c"]}]);
    assert_eq!(legs_of(&object, &cfg()).unwrap().len(), 1);
    assert_eq!(
        legs_of(&bare, &cfg()).unwrap().len(),
        1,
        "`sh([...])` passes its one positional argument through whole"
    );
}

#[test]
fn an_untagged_leg_is_refused_by_name_and_nothing_runs() {
    let args = serde_json::json!({"legs": [{"cmd": "rm -rf /"}]});
    let e = legs_of(&args, &cfg()).expect_err("an untagged leg is refused");
    assert_eq!(e.kind, FailureClass::Denied);
    assert!(
        e.message.contains("rm -rf /") && e.message.contains("3-5 tags"),
        "the refusal must name the leg and the rule: {}",
        e.message
    );
}

#[test]
fn the_colon_string_spelling_still_counts_as_three_tags() {
    let args = serde_json::json!({"legs": [{"cmd": "git status", "tag": "git:status:worktree"}]});
    let legs = legs_of(&args, &cfg()).expect("the older spelling parses");
    assert_eq!(leg_tags(&legs[0].tags), vec!["git", "status", "worktree"]);
}

#[test]
fn more_legs_than_the_bound_are_refused_before_anything_runs() {
    let one = serde_json::json!({"cmd": "true", "tags": ["a", "b", "c"]});
    let args = serde_json::json!({"legs": [one, one, one, one, one]});
    let e = legs_of(&args, &cfg()).expect_err("five legs is past the bound of four");
    assert!(e.message.contains("at most 4 legs"), "{}", e.message);
}

#[tokio::test]
async fn the_results_come_back_in_leg_order_and_a_non_zero_exit_is_data() {
    let dir = tempfile::tempdir().unwrap();
    let legs = vec![
        leg("echo one", serde_json::json!(["echo", "probe", "one"])),
        leg("exit 3", serde_json::json!(["exit", "probe", "three"])),
        leg("echo three", serde_json::json!(["echo", "probe", "three"])),
    ];
    let out = sh(&dir)
        .run_legs(&legs, &CancellationToken::new())
        .await
        .expect("a non-zero exit is data, not a failure");
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].code, 0);
    assert_eq!(out[0].out, "one\n");
    assert_eq!(out[1].code, 3, "the exit code is reported, not thrown");
    assert_eq!(out[2].out, "three\n", "leg order, not completion order");
}

/// The point of the tool: three sleeps that would take 3 s one after another take about 1 s.
/// The bound is deliberately loose — this asserts CONCURRENCY, not a schedule. 1 s legs and a
/// 2.2 s budget rather than 0.5 s and 1.2 s: under a full-suite nextest run this machine's
/// process-spawn latency alone ate the old 700 ms margin (green standalone every time), and the
/// wider gap keeps the sequential case (3 s+) unambiguously OVER the budget.
#[tokio::test]
async fn the_legs_really_do_run_at_the_same_time() {
    let dir = tempfile::tempdir().unwrap();
    let legs: Vec<Leg> = (0..3)
        .map(|i| {
            leg(
                "sleep 1",
                serde_json::json!(["sleep", "probe", format!("leg{i}")]),
            )
        })
        .collect();
    let t0 = std::time::Instant::now();
    let out = sh(&dir)
        .run_legs(&legs, &CancellationToken::new())
        .await
        .expect("three sleeps run");
    let elapsed = t0.elapsed();
    assert!(out.iter().all(|r| r.code == 0));
    assert!(
        elapsed < std::time::Duration::from_millis(2200),
        "three 1 s legs took {elapsed:?}: they ran one after another"
    );
}

#[tokio::test]
async fn a_cancelled_call_stops_its_legs_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let legs = vec![leg(
        "sleep 30",
        serde_json::json!(["sleep", "probe", "cancel"]),
    )];
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        c.cancel();
    });
    let e = sh(&dir)
        .run_legs(&legs, &cancel)
        .await
        .expect_err("a cancelled call does not answer");
    assert_eq!(e.kind, FailureClass::Cancelled);
}

#[tokio::test]
async fn a_leg_that_outruns_the_timeout_is_reported_as_a_leg_and_the_others_still_answer() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = (*cfg()).clone();
    // 2s, not 150ms: under a full-suite nextest run this machine's process-spawn latency alone
    // can exceed 150ms, and then the FAST leg times out too and the test reads as a regression
    // (observed three full runs in a row, 2026-08-29; always green standalone). The budget only
    // needs to sit between "echo" and "sleep 30".
    c.sh_timeout_ms = 2_000;
    let sh = Sh {
        cfg: Arc::new(c),
        root: dir.path().to_path_buf(),
    };
    let legs = vec![
        leg("sleep 30", serde_json::json!(["sleep", "probe", "slow"])),
        leg("echo fast", serde_json::json!(["echo", "probe", "fast"])),
    ];
    let out = sh
        .run_legs(&legs, &CancellationToken::new())
        .await
        .expect("one slow leg does not fail the call");
    assert!(out[0].out.contains("exceeded 2000ms"), "{:?}", out[0]);
    assert_eq!(out[1].out, "fast\n");
}
