//! V9: an example ward DRY-FIRES through the real `bough wards test` binary, printing the actions
//! it WOULD take, and then the same ward file FIRES LIVE on a real ledger step in a booted tree,
//! its actions carried out through the seams — with the spawn bound taken from the `workers`
//! Definition, the claim's cites enforced, and an action kind with no Provider refused by the
//! actions executor while the action after it still runs.
//!
//! The dry half drives the REAL BINARY as a subprocess because half of what is asserted is that
//! `bough wards test` composes, dry-fires and prints; the live half boots the SHIPPED bundles
//! in-process because the assertion is what the seams did.

use crate::support;

use std::path::{Path, PathBuf};
use std::process::Command;

use bough_plugin_agents::{AgentKind, Agents, CreateAgent};
use bough_plugin_hello::trace;
use bough_plugin_ledger::{
    AgentName, Append, Class, Ledger, LedgerHandle, Order, StepQuery, StepType, TrajId, WakeId,
};
use bough_plugin_runtime_actions::{MarkKind, RuntimeAction};
use bough_plugin_wards_rhai::WardFired;
use support::{boot_real, fixture, row_ctx};

/// The example ward under test. It fires on one step type and returns FOUR actions:
/// a spawn, a cited claim, an `act` of a kind whose Provider is not mounted, and a hint after it.
const EXAMPLE_WARD: &str = r#"
fn triggers() { ["thought/text"] }

fn on_event(ev, cx) {
    [
        #{ kind: "spawn", agent: "sol", task: "look into: " + ev.body.text },
        #{ kind: "mark", agent: "sol", mark: "claim", text: "the ward saw a thought",
           cites: ["ledger:traj:" + ev.traj] },
        #{ kind: "act", action_kind: "open_pr", target: "acme/repo", payload: #{} },
        #{ kind: "hint", agent: "sol", text: "and the list kept going" },
    ]
}
"#;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

// ---------------------------------------------------------------------------
// the dry half: `bough wards test <file>` over a ledger with real steps in it
// ---------------------------------------------------------------------------

#[test]
fn bough_wards_test_dry_fires_over_real_ledger_steps_and_prints_would_do_actions() {
    let home = std::env::temp_dir().join(format!(
        "bough-wards-v9-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();

    // Real steps, written by a real run: `bough exec` against the replay adapter fills
    // `$BOUGH_HOME/ledger.db` with the wake's own chain.
    let exec = Command::new(env!("CARGO_BIN_EXE_bough"))
        .env("BOUGH_HOME", &home)
        .env("HOME", &home)
        .arg("--root")
        .arg(repo_root())
        .arg("--patch")
        .arg(fixture("exec-replay.yml"))
        .arg("exec")
        .arg("what is two plus two")
        .output()
        .expect("the bough binary runs");
    assert!(
        exec.status.success(),
        "seeding the ledger failed: {}",
        String::from_utf8_lossy(&exec.stderr)
    );

    let ward = home.join("example.rhai");
    std::fs::write(&ward, EXAMPLE_WARD).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_bough"))
        .env("BOUGH_HOME", &home)
        .env("HOME", &home)
        .arg("--root")
        .arg(repo_root())
        .arg("--profile")
        .arg("headless")
        .arg("--no-watch")
        .arg("wards")
        .arg("test")
        .arg(&ward)
        .output()
        .expect("the bough binary runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "`bough wards test` must exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );

    // It considered the steps the seeding run actually wrote, and it says what it WOULD do.
    assert!(
        stdout.contains("ward `example`:"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.contains("0 would fire"),
        "the ward saw no `thought/text` step to dry-fire on\nstdout: {stdout}"
    );
    for expected in [
        "would spawn a worker on `sol`",
        "would mark Claim on `sol`",
        "would act open_pr on acme/repo",
        "would hint `sol`",
    ] {
        assert!(
            stdout.contains(expected),
            "the dry run must print `{expected}`\nstdout: {stdout}"
        );
    }
    // A dry run TOUCHES NO SEAM: nothing it would have done reached the ledger.
    let after = Command::new(env!("CARGO_BIN_EXE_bough"))
        .env("BOUGH_HOME", &home)
        .env("HOME", &home)
        .arg("--root")
        .arg(repo_root())
        .arg("--profile")
        .arg("headless")
        .arg("--no-watch")
        .arg("wards")
        .arg("test")
        .arg(&ward)
        .arg("--print")
        .arg("json")
        .output()
        .expect("the bough binary runs");
    let json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&after.stdout).trim())
            .expect("`--print json` prints json");
    assert_eq!(json["ward"], "example");
    assert!(
        json["errors"].as_array().unwrap().is_empty(),
        "the dry run reported errors: {json}"
    );
    // No `ward/fired`, no `claim/proposed`: two dry runs left the chain exactly as the seeding
    // run did.
    let db = std::fs::read(home.join("ledger.db")).expect("the ledger is on disk");
    let text = String::from_utf8_lossy(&db);
    assert!(
        !text.contains("ward/fired") && !text.contains("claim/proposed"),
        "a dry run wrote to the ledger"
    );

    let _ = std::fs::remove_dir_all(&home);
}

// ---------------------------------------------------------------------------
// the live half: the same file, mounted, firing on a real step
// ---------------------------------------------------------------------------

/// The patch the live half boots under: the ward directory is this test's own, and
/// `actions.github` is DISABLED so `open_pr` has no Provider — which is what makes the refusal
/// come from the executor rather than from a real `gh` invocation. Nothing outward-facing can run.
fn live_patch(dir: &Path, wards: &Path, max_spawns: usize, max_firings: u32) -> PathBuf {
    let yaml = format!(
        "\
entries:
  actions.github:
    disabled: true
  wards:
    config:
      dir: {wards}
      glob: \"*.rhai\"
      watch: false
      debounce_ms: 400
      max_ops: 200000
      max_depth: 32
      max_string_bytes: 65536
      max_array_size: 4096
      eval_timeout_ms: 2000
      max_firings_per_minute: {max_firings}
      limits: {{ max_actions: 16, max_spawns: {max_spawns}, max_text_bytes: 8192 }}
",
        wards = wards.display()
    );
    let path = dir.join(format!("wards-live-{max_spawns}-{max_firings}.yml"));
    std::fs::write(&path, yaml).unwrap();
    path
}

/// A scratch directory holding this test's ward file and its patch layer.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str, ward: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!(
            "bough-wards-live-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(p.join("wards")).unwrap();
        std::fs::write(p.join("wards").join("example.rhai"), ward).unwrap();
        Scratch(p)
    }
    fn wards(&self) -> PathBuf {
        self.0.join("wards")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Boot the shipped headless tree with this ward mounted, create a live `sol`, and append one real
/// step to its lane. Returns the ledger, the appended step and the kernel.
async fn fire(
    scratch: &Scratch,
    max_spawns: usize,
    max_firings: u32,
    text: &str,
) -> (
    std::sync::Arc<bough_kernel::Kernel>,
    support::TempDir,
    LedgerHandle,
    bough_plugin_ledger::Step,
) {
    let patch = live_patch(&scratch.0, &scratch.wards(), max_spawns, max_firings);
    let (kernel, dir) = boot_real("headless", &[fixture("llm-replay.yml"), patch]).await;

    let ctx = row_ctx(&kernel, "exec");
    let agents = ctx.get::<Agents>().expect("the agents key is bound");
    let ledger = LedgerHandle(
        ctx.get::<Ledger>()
            .expect("the ledger key is bound")
            .0
            .clone(),
    );

    let traj = TrajId::new("lane/sol");
    let (_sol, _disposer) = agents
        .create(CreateAgent {
            name: AgentName::new("sol"),
            traj: traj.clone(),
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: chrono::Utc::now(),
        })
        .await
        .expect("a live `sol`");
    // The disposer is deliberately leaked for the test's lifetime: dropping it would retire the
    // agent the ward is about to act on.
    std::mem::forget(_disposer);

    let step = ledger
        .0
        .append(Append {
            traj,
            wake: WakeId::new("wake:v9"),
            kind: StepType::new("thought/text"),
            class: Class::Thought,
            body: serde_json::json!({ "text": text, "step_index": 0 }),
            cites: vec![],
            at: chrono::Utc::now(),
            id: None,
        })
        .await
        .expect("the step appends");
    (kernel, dir, ledger, step)
}

/// Poll for the `ward/fired` row the firing writes.
async fn fired(ledger: &LedgerHandle) -> WardFired {
    for _ in 0..200 {
        if let Ok(steps) = ledger
            .0
            .steps(&StepQuery {
                kinds: vec![StepType::new("ward/fired")],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
        {
            if let Some(s) = steps.into_iter().next() {
                return serde_json::from_value((*s.body).clone())
                    .expect("the `ward/fired` body parses");
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("the ward never fired");
}

/// The DURABLE record that a spawn reached `ctx.workers`: `worker/started`, appended by the seam
/// itself into the SPAWNER's chain.
///
/// `Workers::live()` was the obvious thing to assert on and it is the wrong thing: it lists the
/// workers running RIGHT NOW, so it answers 1 or 0 depending on whether the worker has finished
/// yet, which is a race against the machine's load rather than a fact about the spawn.
async fn worker_starts(ledger: &LedgerHandle) -> Vec<bough_plugin_ledger::Step> {
    for _ in 0..200 {
        if let Ok(steps) = ledger
            .0
            .steps(&StepQuery {
                kinds: vec![StepType::new("worker/started")],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
        {
            if !steps.is_empty() {
                return steps;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    Vec::new()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_example_ward_fires_on_a_real_ledger_step_and_its_actions_execute_through_the_seams() {
    let _guard = trace::test_lock();
    let scratch = Scratch::new("all", EXAMPLE_WARD);
    let (kernel, _dir, ledger, step) = fire(&scratch, 2, 1, "the build is red").await;

    let body = fired(&ledger).await;
    assert_eq!(body.ward, "example");
    assert_eq!(body.on, step.seq, "it fired on the step that was appended");
    assert_eq!(
        body.actions.len(),
        4,
        "the journal carries what `evaluate` returned: {:?}",
        body.actions
    );
    assert_eq!(
        body.actions[0],
        RuntimeAction::Spawn {
            agent: "sol".into(),
            task: "look into: the build is red".into(),
            tools: None,
        }
    );

    // 1. the spawn REACHED `ctx.workers`: a run exists that the seam itself reports.
    assert!(
        body.outcomes[0].starts_with("did: spawned worker"),
        "outcomes: {:?}",
        body.outcomes
    );
    let starts = worker_starts(&ledger).await;
    assert_eq!(
        starts.len(),
        1,
        "the spawn must exist on the workers seam, not merely in the ward's journal"
    );
    assert_eq!(
        starts[0].traj,
        bough_plugin_ledger::TrajId::new("lane/sol"),
        "the seam records the start in the SPAWNER's chain"
    );

    // 2. the claim REACHED `ctx.ledger`, carrying the cite the ward supplied.
    assert!(
        body.outcomes[1].starts_with("did:"),
        "outcomes: {:?}",
        body.outcomes
    );
    let claims = ledger
        .0
        .steps(&StepQuery {
            kinds: vec![StepType::new("claim/proposed")],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the ledger reads");
    let fired_rows = ledger
        .0
        .steps(&StepQuery {
            kinds: vec![StepType::new("ward/fired")],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the ledger reads");
    let where_from: Vec<String> = claims
        .iter()
        .map(|c| format!("{}@{:?}", c.traj.as_str(), c.seq))
        .collect();
    // ONE claim, because ONE firing: `fire` sets `max_firings_per_minute: 1`. Without that bound
    // this ward is a LOOP -- its `hint` wakes `sol`, `sol`'s reply is another `thought/text`, and
    // the ward triggers on `thought/text`. It fired 5 times in 3 seconds and climbing before the
    // host grew the rate bound, which is what made this assertion flaky under load.
    assert_eq!(
        claims.len(),
        1,
        "one claim, appended by the executor; firings={} claims_at={:?}",
        fired_rows.len(),
        where_from
    );
    assert_eq!(
        fired_rows.len(),
        1,
        "the rate bound cut the loop after exactly one firing"
    );
    let cites: Vec<String> = claims[0]
        .cites
        .iter()
        .map(|c| c.r#ref.as_str().to_string())
        .collect();
    assert_eq!(
        cites,
        vec!["ledger:traj:lane/sol".to_string()],
        "the claim carries the ward's cite"
    );

    // 3. `open_pr` has no Provider in this tree, so the ACTIONS EXECUTOR refused it — the write
    //    boundary, not the script — and 4. the action after the refusal still ran.
    assert!(
        body.outcomes[2].starts_with("refused:"),
        "an action kind with no Provider must be refused: {:?}",
        body.outcomes
    );
    assert!(
        body.outcomes[3].starts_with("did:") || body.outcomes[3].starts_with("refused:"),
        "outcomes: {:?}",
        body.outcomes
    );
    assert_eq!(body.outcomes.len(), 4, "outcomes: {:?}", body.outcomes);

    kernel.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ward_mark_without_cites_is_refused_and_no_claim_is_written() {
    let _guard = trace::test_lock();
    const UNCITED: &str = r#"
fn triggers() { ["thought/text"] }
fn on_event(ev, cx) {
    [ #{ kind: "mark", agent: "sol", mark: "claim", text: "trust me", cites: [] } ]
}
"#;
    let scratch = Scratch::new("uncited", UNCITED);
    let (kernel, _dir, ledger, _step) = fire(&scratch, 2, 1, "no evidence here").await;

    let body = fired(&ledger).await;
    assert_eq!(
        body.actions[0],
        RuntimeAction::Mark {
            agent: "sol".into(),
            mark: MarkKind::Claim,
            text: "trust me".into(),
            cites: vec![],
        }
    );
    assert!(
        body.outcomes[0].contains("must cite its evidence"),
        "an uncited claim must be refused at the boundary: {:?}",
        body.outcomes
    );
    let claims = ledger
        .0
        .steps(&StepQuery {
            kinds: vec![StepType::new("claim/proposed")],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the ledger reads");
    assert!(
        claims.is_empty(),
        "a refused claim must not be on the chain: {claims:?}"
    );
    kernel.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ward_spawn_is_bounded_by_the_hosts_limits_not_by_the_script() {
    let _guard = trace::test_lock();
    const GREEDY: &str = r#"
fn triggers() { ["thought/text"] }
fn on_event(ev, cx) {
    [
        #{ kind: "spawn", agent: "sol", task: "one" },
        #{ kind: "spawn", agent: "sol", task: "two" },
        #{ kind: "spawn", agent: "sol", task: "three" },
    ]
}
"#;
    let scratch = Scratch::new("greedy", GREEDY);
    // `max_spawns: 1` — the HOST's bound, which the script cannot see or raise.
    let (kernel, _dir, ledger, _step) = fire(&scratch, 1, 1, "spawn everything").await;

    let body = fired(&ledger).await;
    assert_eq!(body.actions.len(), 3, "the ward asked for three spawns");
    let did = body
        .outcomes
        .iter()
        .filter(|o| o.starts_with("did: spawned worker"))
        .count();
    assert_eq!(
        did, 1,
        "exactly one spawn survived the bound: {:?}",
        body.outcomes
    );
    assert!(
        body.outcomes.iter().any(|o| o.contains("max_spawns is 1")),
        "the bound must be REPORTED, not silent: {:?}",
        body.outcomes
    );
    assert_eq!(
        worker_starts(&ledger).await.len(),
        1,
        "the workers seam saw exactly one start"
    );
    kernel.shutdown().await;
}
