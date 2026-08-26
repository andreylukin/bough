//! §17 Phase 6 SWAP gate, the three swaps of the verification map, driven through the LAUNCHER'S
//! OWN recompose (`bough::watch::recompose_once`) over the SHIPPED `profiles/` and `bundles/`.
//!
//! Every claim here is behavioural, never structural:
//!
//! * the collector's job is FIRED (`Scheduler::fire_now`) and its mail is read back off the
//!   ledger, so "sweeps stop" means a fire that can no longer happen and a `gh` process that is
//!   no longer spawned — not a `disabled: true` flag in a dump;
//! * the ward children are counted as ROWS and as `ledger/step` LISTENERS, so "no listeners
//!   remain" is read off the live event core;
//! * the sleep listener is replaced by `power-test` and a synthetic wake is fired through the
//!   `/power` command, so "catch-up still works" means `catch-up-on-wake` really called
//!   `Agent::request_wake` through the agents seam.
//!
//! The `gh` the collector runs is `scripts/fixtures/gh/gh`, the recording shim: an unplanned `gh`
//! call is a red test, never a network request.

mod support;

use std::path::{Path, PathBuf};

use bough_kernel::{FiberState, Kernel, RowSnapshot};
use bough_plugin_agents::{Agents, MailClass, Message, MessageId, Sender, Target};
use bough_plugin_commands::{CommandCx, CommandName, Commands, Invocation};
use bough_plugin_hello::trace;
use bough_plugin_ledger::query::{Order, StepQuery};
use bough_plugin_ledger::{AgentName, Ledger, Step, StepType, TrajId};
use bough_plugin_power::Power;
use bough_plugin_schedule::{JobName, JobOutcome, Schedule, ScheduleError};
use support::{boot_real, clear_patch, fixture, recompose, row, write_patch};

// ---------------------------------------------------------------------------------------------
// shared
// ---------------------------------------------------------------------------------------------

/// Every row in the tree, flattened, as `(id, state)`. "Nothing else in the tree changed" is
/// asserted against this.
fn all_rows(kernel: &Kernel) -> Vec<(String, FiberState, Option<u64>)> {
    fn walk(rows: &[RowSnapshot], out: &mut Vec<(String, FiberState, Option<u64>)>) {
        for r in rows {
            out.push((r.id.as_str().to_string(), r.state, r.uid.map(|u| u.0)));
            walk(&r.children, out);
        }
    }
    let mut out = Vec::new();
    walk(&kernel.snapshot().rows, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn no_row_failed(kernel: &Kernel) {
    let failed: Vec<String> = all_rows(kernel)
        .into_iter()
        .filter(|(_, s, _)| *s == FiberState::Failed)
        .map(|(id, _, _)| id)
        .collect();
    assert!(failed.is_empty(), "rows FAILED: {failed:?}");
}

fn schedule(kernel: &Kernel) -> bough_plugin_schedule::ScheduleHandle {
    (*kernel
        .root()
        .peek_live::<Schedule>()
        .expect("`schedule` is bound"))
    .clone()
}

fn job_names(kernel: &Kernel) -> Vec<String> {
    let mut v: Vec<String> = schedule(kernel)
        .0
        .jobs()
        .into_iter()
        .map(|j| j.name.to_string())
        .collect();
    v.sort();
    v
}

/// The resident the shipped tree already created, with the trajectory the ledger says is its
/// own. The `residents` row mounts `sol`; a test that made a second one would be testing its own
/// fixture rather than the tree.
async fn resident(kernel: &Kernel, name: &str) -> (bough_plugin_agents::Agent, TrajId) {
    let agents = kernel
        .root()
        .peek_live::<Agents>()
        .expect("`agents` is bound");
    let ledger = kernel
        .root()
        .peek_live::<Ledger>()
        .expect("`ledger` is bound");
    let agent_name = AgentName::new(name);
    let agent = agents
        .by_name(&agent_name)
        .unwrap_or_else(|| panic!("the shipped tree has no resident `{name}`"));
    let traj = ledger
        .0
        .agent(&agent_name)
        .await
        .expect("a read")
        .expect("the agent has a ledger row")
        .traj;
    (agent, traj)
}

async fn delivered(kernel: &Kernel, traj: &TrajId) -> Vec<Step> {
    let ledger = kernel
        .root()
        .peek_live::<Ledger>()
        .expect("`ledger` is bound");
    ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj.clone()],
            kinds: vec![StepType::new("mail/delivered")],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the chain reads back")
}

/// A patch file of our own, outside `$BOUGH_HOME`, passed as `--patch` at boot.
fn write_layer(dir: &Path, name: &str, yaml: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, yaml).expect("the layer is writable");
    p
}

fn scratch(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("bough-p6swap-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("a scratch dir");
    p
}

// ---------------------------------------------------------------------------------------------
// 1. the collector row
// ---------------------------------------------------------------------------------------------

/// The `gh` shim's canonical fixture name: argv joined by spaces, every character outside
/// `[A-Za-z0-9._-]` replaced by `_`. The same string `scripts/fixtures/gh/gh` computes.
fn fixture_name(argv: &str) -> String {
    argv.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

const PR_ARGV: &str =
    "pr list --repo o/r --json number,title,url,updatedAt,author,state,isDraft --limit 50";

fn prs_json(numbers: &[u64]) -> String {
    let rows: Vec<String> = numbers
        .iter()
        .map(|n| {
            format!(
                r#"{{"number":{n},"title":"PR {n}","url":"https://example.invalid/{n}",
                    "updatedAt":"2026-08-0{n}T00:00:00Z","author":{{"login":"andrey"}},
                    "state":"OPEN","isDraft":false}}"#
            )
        })
        .collect();
    format!("[{}]", rows.join(","))
}

/// A collector pointed at the recording shim, one repo, delivering to `sol`, with its watermark
/// file in this test's own scratch directory.
fn collector_layer(scratch: &Path, shim: &Path) -> String {
    format!(
        "entries:\n  collect.github:\n    config:\n      cadence: {{ every_ms: 3600000 }}\n      \
         gh_bin: {gh}\n      repos: [\"o/r\"]\n      prs: true\n      review_requests: false\n      \
         mentions: false\n      checks: false\n      deliver_to: [sol]\n      \
         wake_classes: [review_request]\n      known_bots: []\n      state_db: {db}\n      \
         batch: 50\n      timeout_ms: 30000\n",
        gh = shim.display(),
        db = scratch.join("collect-github.db").display(),
    )
}

const DISABLE_COLLECTOR: &str = "entries:\n  collect.github:\n    disabled: true\n";

fn shim_path() -> PathBuf {
    support::repo_root().join("scripts/fixtures/gh/gh")
}

#[tokio::test(flavor = "multi_thread")]
async fn disabling_the_row_by_patch_stops_sweeps_and_removes_its_schedule_job() {
    let _guard = trace::test_lock();
    let scratch = scratch("collector");
    let shim_dir = scratch.join("gh");
    std::fs::create_dir_all(&shim_dir).unwrap();
    std::fs::write(
        shim_dir.join(format!("{}.json", fixture_name(PR_ARGV))),
        prs_json(&[1, 2]),
    )
    .unwrap();
    let log = scratch.join("argv.log");
    // SAFETY: this test holds the process-wide test lock.
    unsafe {
        std::env::set_var("GH_SHIM_DIR", &shim_dir);
        std::env::set_var("GH_SHIM_LOG", &log);
    }

    let layer = write_layer(
        &scratch,
        "collector.yml",
        &collector_layer(&scratch, &shim_path()),
    );
    let (kernel, dir) = boot_real("tui", &[fixture("llm-replay.yml"), layer.clone()]).await;
    let (_sol, traj) = resident(&kernel, "sol").await;

    // --- the row sweeps, for real, through the schedule seam -----------------------------------
    assert!(
        job_names(&kernel).contains(&"collector-github".to_string()),
        "{:?}",
        job_names(&kernel)
    );
    let run = schedule(&kernel)
        .0
        .fire_now(&JobName::new("collector-github"))
        .await
        .expect("the job fires");
    assert!(
        matches!(run.outcome, JobOutcome::Ran { .. }),
        "{:?}",
        run.outcome
    );
    let before = delivered(&kernel, &traj).await;
    assert_eq!(before.len(), 2, "two PRs, one agent: {before:?}");
    assert!(before
        .iter()
        .all(|s| s.refs.iter().any(|r| r.as_str().starts_with("gh:"))));
    let gh_calls_before = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .count();
    assert!(gh_calls_before > 0, "the shim really ran");

    let rows_before = all_rows(&kernel);
    let jobs_before = job_names(&kernel);

    // --- the swap ------------------------------------------------------------------------------
    write_patch(&dir, DISABLE_COLLECTOR);
    recompose(&kernel, "", &dir)
        .await
        .expect("the swap composes");

    assert_eq!(row(&kernel, "collect.github").state, FiberState::Inactive);
    assert!(
        !job_names(&kernel).contains(&"collector-github".to_string()),
        "the job left with its row: {:?}",
        job_names(&kernel)
    );
    // …and firing it is now impossible: the sweep cannot happen, not merely does not.
    match schedule(&kernel)
        .0
        .fire_now(&JobName::new("collector-github"))
        .await
    {
        Err(ScheduleError::Unknown(n)) => assert_eq!(n.to_string(), "collector-github"),
        other => panic!("a disabled collector must have no job to fire: {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .count(),
        gh_calls_before,
        "not one `gh` was spawned after the swap"
    );
    assert_eq!(delivered(&kernel, &traj).await.len(), 2, "no further mail");

    // --- nothing else in the tree changed ------------------------------------------------------
    let rows_after = all_rows(&kernel);
    let changed: Vec<_> = rows_before
        .iter()
        .zip(rows_after.iter())
        .filter(|(a, b)| a != b)
        .collect();
    assert_eq!(
        rows_before.len(),
        rows_after.len(),
        "no row appeared or left"
    );
    assert_eq!(changed.len(), 1, "exactly one row changed: {changed:#?}");
    assert_eq!(changed[0].0 .0, "collect.github");
    let mut jobs_after = job_names(&kernel);
    jobs_after.push("collector-github".to_string());
    jobs_after.sort();
    assert_eq!(jobs_after, jobs_before, "every other job stayed registered");
    no_row_failed(&kernel);

    kernel.shutdown().await;
    let _ = std::fs::remove_dir_all(&scratch);
}

#[tokio::test(flavor = "multi_thread")]
async fn re_enabling_resumes_from_the_watermark_with_no_duplicates() {
    let _guard = trace::test_lock();
    let scratch = scratch("collector-resume");
    let shim_dir = scratch.join("gh");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let fixture_file = shim_dir.join(format!("{}.json", fixture_name(PR_ARGV)));
    std::fs::write(&fixture_file, prs_json(&[1, 2])).unwrap();
    // SAFETY: this test holds the process-wide test lock.
    unsafe {
        std::env::set_var("GH_SHIM_DIR", &shim_dir);
        std::env::set_var("GH_SHIM_LOG", scratch.join("argv.log"));
    }

    let layer = write_layer(
        &scratch,
        "collector.yml",
        &collector_layer(&scratch, &shim_path()),
    );
    let (kernel, dir) = boot_real("tui", &[fixture("llm-replay.yml"), layer]).await;
    let (_sol, traj) = resident(&kernel, "sol").await;

    schedule(&kernel)
        .0
        .fire_now(&JobName::new("collector-github"))
        .await
        .expect("the first sweep");
    assert_eq!(delivered(&kernel, &traj).await.len(), 2);

    write_patch(&dir, DISABLE_COLLECTOR);
    recompose(&kernel, "", &dir)
        .await
        .expect("the swap composes");

    // The world moves on while the row is off: a third PR appears.
    std::fs::write(&fixture_file, prs_json(&[1, 2, 3])).unwrap();

    clear_patch(&dir);
    recompose(&kernel, "", &dir)
        .await
        .expect("removing the patch composes");
    assert_eq!(row(&kernel, "collect.github").state, FiberState::Active);
    assert!(job_names(&kernel).contains(&"collector-github".to_string()));

    let run = schedule(&kernel)
        .0
        .fire_now(&JobName::new("collector-github"))
        .await
        .expect("the resumed sweep");
    assert!(
        matches!(run.outcome, JobOutcome::Ran { .. }),
        "{:?}",
        run.outcome
    );

    let after = delivered(&kernel, &traj).await;
    let refs: Vec<String> = after
        .iter()
        .flat_map(|s| s.refs.iter().map(|r| r.as_str().to_string()))
        .filter(|r| r.starts_with("gh:"))
        .collect();
    assert_eq!(
        after.len(),
        3,
        "the two already-delivered PRs are NOT delivered again: {refs:?}"
    );
    assert!(refs.contains(&"gh:o/r#3".to_string()), "{refs:?}");
    let mut sorted = refs.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), refs.len(), "no duplicate ref: {refs:?}");
    no_row_failed(&kernel);

    kernel.shutdown().await;
    let _ = std::fs::remove_dir_all(&scratch);
}

// ---------------------------------------------------------------------------------------------
// 2. the ward host row
// ---------------------------------------------------------------------------------------------

const WARD: &str = "fn on_event(ev, cx) { [] }\n";

fn wards_layer(dir: &Path) -> String {
    format!(
        "entries:\n  wards:\n    config:\n      dir: {dir}\n      glob: \"*.rhai\"\n      \
         watch: true\n      debounce_ms: 400\n      max_ops: 200000\n      max_depth: 32\n      \
         max_string_bytes: 65536\n      max_array_size: 4096\n      eval_timeout_ms: 2000\n      \
         max_firings_per_minute: 60\n      limits: {{ max_actions: 16, max_spawns: 2, max_text_bytes: 8192 }}\n",
        dir = dir.display()
    )
}

const DISABLE_WARDS: &str = "entries:\n  wards:\n    disabled: true\n";

fn ward_rows(kernel: &Kernel) -> Vec<String> {
    fn walk(rows: &[RowSnapshot], out: &mut Vec<String>) {
        for r in rows {
            if r.plugin.as_deref() == Some("ward") {
                out.push(r.id.as_str().to_string());
            }
            walk(&r.children, out);
        }
    }
    let mut out = Vec::new();
    walk(&kernel.snapshot().rows, &mut out);
    out.sort();
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn disabling_the_host_row_unmounts_every_ward_child_entry() {
    let _guard = trace::test_lock();
    let scratch = scratch("wards");
    let ward_dir = scratch.join("wards");
    std::fs::create_dir_all(&ward_dir).unwrap();
    std::fs::write(ward_dir.join("a.rhai"), WARD).unwrap();
    std::fs::write(ward_dir.join("b.rhai"), WARD).unwrap();

    let layer = write_layer(&scratch, "wards.yml", &wards_layer(&ward_dir));
    let (kernel, dir) = boot_real("tui", &[fixture("llm-replay.yml"), layer]).await;

    assert_eq!(
        ward_rows(&kernel),
        vec!["wards.a".to_string(), "wards.b".to_string()],
        "one child entry per ward file"
    );
    let listeners_with_wards = kernel.core().listener_count("ledger/step");
    let rows_before = all_rows(&kernel);

    write_patch(&dir, DISABLE_WARDS);
    recompose(&kernel, "", &dir)
        .await
        .expect("the swap composes");

    assert!(ward_rows(&kernel).is_empty(), "{:?}", ward_rows(&kernel));
    assert_eq!(row(&kernel, "wards").state, FiberState::Inactive);
    assert_eq!(
        kernel.core().listener_count("ledger/step"),
        listeners_with_wards - 2,
        "each retired ward gave back its `ledger/step` listener"
    );
    no_row_failed(&kernel);

    // The two ward children are the ONLY rows that left.
    let after = all_rows(&kernel);
    let gone: Vec<String> = rows_before
        .iter()
        .map(|(id, _, _)| id.clone())
        .filter(|id| !after.iter().any(|(a, _, _)| a == id))
        .collect();
    assert_eq!(gone, vec!["wards.a".to_string(), "wards.b".to_string()]);

    // --- and re-enabling returns them ----------------------------------------------------------
    clear_patch(&dir);
    recompose(&kernel, "", &dir)
        .await
        .expect("removing the patch composes");

    assert_eq!(
        ward_rows(&kernel),
        vec!["wards.a".to_string(), "wards.b".to_string()],
        "every ward came back"
    );
    assert_eq!(row(&kernel, "wards").state, FiberState::Active);
    assert_eq!(
        kernel.core().listener_count("ledger/step"),
        listeners_with_wards,
        "the listeners came back with them"
    );
    no_row_failed(&kernel);

    kernel.shutdown().await;
    let _ = std::fs::remove_dir_all(&scratch);
}

// ---------------------------------------------------------------------------------------------
// 3. the sleep listener
// ---------------------------------------------------------------------------------------------

/// `power.sleep` KEEPS ITS ID and changes its plugin: the row is the seat, the Provider is what
/// sits in it. `power-test` is in the catalog and in no bundle, so this patch is the only thing
/// that ever mounts it.
const POWER_TEST: &str =
    "entries:\n  power.sleep:\n    plugin: power-test\n    config: { command: true }\n";

fn mail(text: &str) -> Message {
    Message {
        id: MessageId::new(format!("msg-{text}")),
        // ORDINARY mail from a collector: an Andrey message or wake-class mail would be woken on
        // IMMEDIATELY by the loop's own `notify`, and the catch-up would have nothing left to read.
        from: Sender::Collector("phase6-swap-test".to_string()),
        class: MailClass::Ordinary,
        text: text.to_string(),
        subject: text.to_string(),
        cites: Vec::new(),
        refs: Default::default(),
        mail_seq: None,
        at: chrono::Utc::now(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn replacing_sleep_listener_with_power_test_by_patch_keeps_catch_up_working() {
    let _guard = trace::test_lock();
    let scratch = scratch("power");
    let layer = write_layer(&scratch, "power-test.yml", POWER_TEST);
    let (kernel, _dir) = boot_real("tui", &[fixture("llm-replay.yml"), layer]).await;

    // The seat is filled by the TEST Provider, and the consumer row mounted against it.
    assert_eq!(row(&kernel, "power.sleep").state, FiberState::Active);
    assert_eq!(row(&kernel, "catch-up.on-wake").state, FiberState::Active);
    let source = kernel
        .root()
        .peek_live::<Power>()
        .expect("`power` is bound");
    assert_eq!(source.0.kind(), "test", "the swapped-in Provider is live");
    no_row_failed(&kernel);

    // An agent with QUEUED MAIL: `request_wake` answers `Nothing` when there is nothing to read,
    // so the catch-up is only observable over mail that is actually waiting.
    let ctx = kernel.root().clone();
    let (agent, traj) = resident(&kernel, "sol").await;
    // Queued for the NEXT WAKE and explicitly NOT waking: the only thing that can open a wake
    // here is the synthetic power event below, which is the whole point of the test.
    agent
        .send(mail("overnight"), Target::NextWake, false)
        .await
        .expect("mail lands");

    bough_plugin_catch_up_on_wake::invariant::clear();

    // Fire a synthetic wake the way a human would: through the `/power` command the test Provider
    // registers. Nothing here calls `catch-up-on-wake` directly.
    let commands = kernel
        .root()
        .peek_live::<Commands>()
        .expect("`commands` is bound");
    commands
        .dispatch(
            Invocation {
                name: CommandName::new("power"),
                raw: "/power wake 3600".to_string(),
                args: vec!["wake".to_string(), "3600".to_string()],
            },
            CommandCx {
                ctx: ctx.clone(),
                agent: None,
                at: chrono::Utc::now(),
            },
        )
        .await
        .expect("the synthetic wake dispatches");

    // `catch-up-on-wake` asked the agents seam for a wake, for this agent, and it STARTED.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let seen = loop {
        let seen = bough_plugin_catch_up_on_wake::invariant::seen();
        if !seen.is_empty() || std::time::Instant::now() >= deadline {
            break seen;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    };
    let for_sol: Vec<_> = seen.iter().filter(|o| o.agent == *agent.id()).collect();
    assert_eq!(
        for_sol.len(),
        1,
        "exactly one catch-up wake for the one active agent: {seen:?}"
    );
    assert!(
        for_sol[0].started,
        "the queued mail was actually read: {:?}",
        for_sol[0]
    );

    // …and the wake it opened runs to completion rather than hanging.
    tokio::time::timeout(std::time::Duration::from_secs(30), agent.when_idle())
        .await
        .expect("the catch-up wake finished");
    let _ = &traj;
    no_row_failed(&kernel);

    kernel.shutdown().await;
    let _ = std::fs::remove_dir_all(&scratch);
}
