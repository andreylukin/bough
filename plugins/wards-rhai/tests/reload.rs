//! Invariant under test (P6-D11): a ward file is a ROW, and hot reload moves EXACTLY one of them.
//! Editing one file disposes and remounts that child alone — every other child keeps its fiber uid
//! — and the tree's `ledger/step` listener count returns to what it was, so a reload leaves no
//! trace behind (§0.2).

use bough_plugin_ledger_memory as _;
use bough_plugin_wards_rhai as _;

use std::path::Path;
use std::sync::Arc;

use bough_kernel::{
    Catalog, Composer, Composition, ExprEnv, Kernel, KernelOptions, LayerId, Patch, RowSnapshot,
};

const WARD_A: &str = "fn on_event(ev, cx) { [] }\n";
const WARD_B: &str = "fn on_event(ev, cx) { [] }\n";
const WARD_B_EDITED: &str = "fn on_event(ev, cx) { let x = 1; [] }\n";

fn tree(dir: &Path) -> String {
    format!(
        "\
- id: ledger
  plugin: ledger-memory
- id: agents
  plugin: agents
  inject: [ledger]
  config: {{}}
- id: workers
  plugin: workers
  config: {{ max_in_flight: 8, max_depth: 3, per_wake_spawn_cap: 4 }}
- id: actions
  plugin: actions
  inject: [ledger]
  config: {{}}
- id: schedule
  plugin: schedule-null
  config: {{}}
- id: wards
  plugin: wards-rhai
  inject: [ledger, workers, actions, agents, schedule]
  config:
    dir: {dir}
    glob: \"*.rhai\"
    watch: true
    debounce_ms: 30
    max_ops: 100000
    max_depth: 16
    max_string_bytes: 10000
    max_array_size: 100
    eval_timeout_ms: 1000
    limits: {{ max_actions: 8, max_spawns: 1, max_text_bytes: 4000 }}
",
        dir = dir.display()
    )
}

async fn boot(yaml: &str) -> Arc<Kernel> {
    let catalog = Catalog::from_inventory().expect("the linked catalog has no duplicate names");
    let patch: Patch = serde_yaml::from_str(yaml).expect("the test bundle parses");
    let mut composer = Composer::new(&catalog, ExprEnv::new("test"));
    composer.layer(LayerId::new("test"), patch);
    let composition: Composition = composer.compose().expect("the test bundle composes");
    let kernel = Kernel::new(
        catalog,
        KernelOptions {
            profile: "test".into(),
            invariants: false,
        },
    );
    kernel.load(composition).await.expect("the tree mounts");
    kernel.quiesce().await;
    kernel
}

/// The ward children, by row id, with the fiber uid each currently has.
fn wards(kernel: &Kernel) -> Vec<(String, u64)> {
    fn walk(rows: &[RowSnapshot], out: &mut Vec<(String, u64)>) {
        for r in rows {
            if r.plugin.as_deref() == Some("ward") {
                out.push((r.id.as_str().to_string(), r.uid.map(|u| u.0).unwrap_or(0)));
            }
            walk(&r.children, out);
        }
    }
    let mut out = Vec::new();
    walk(&kernel.rows_snapshot(), &mut out);
    out.sort();
    out
}

/// Wait until `f` holds or the deadline passes; a reload is debounced, so a test waits on the
/// CONDITION rather than on a sleep.
async fn until(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if f() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn editing_one_ward_file_remounts_exactly_that_child() {
    let dir = tempfile::tempdir().expect("a temp wards dir");
    std::fs::write(dir.path().join("a.rhai"), WARD_A).unwrap();
    std::fs::write(dir.path().join("b.rhai"), WARD_B).unwrap();

    let kernel = boot(&tree(dir.path())).await;
    let before = wards(&kernel);
    assert_eq!(
        before.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
        vec!["ward.a".to_string(), "ward.b".to_string()],
        "one child entry per ward file"
    );
    // One listener per ward, on top of whatever the ledger row itself keeps.
    let listeners_before = kernel.core().listener_count("ledger/step");

    std::fs::write(dir.path().join("b.rhai"), WARD_B_EDITED).unwrap();

    let changed = until(|| {
        wards(&kernel).iter().any(|(id, uid)| {
            id == "ward.b"
                && Some(*uid) != before.iter().find(|(i, _)| i == "ward.b").map(|(_, u)| *u)
        })
    })
    .await;
    assert!(
        changed,
        "the edited ward never remounted: {:?}",
        wards(&kernel)
    );

    let after = wards(&kernel);
    assert_eq!(
        after.len(),
        2,
        "the tree still has exactly two wards: {after:?}"
    );
    let uid_of = |v: &[(String, u64)], id: &str| v.iter().find(|(i, _)| i == id).map(|(_, u)| *u);
    assert_eq!(
        uid_of(&after, "ward.a"),
        uid_of(&before, "ward.a"),
        "the untouched ward kept its fiber"
    );
    assert_ne!(
        uid_of(&after, "ward.b"),
        uid_of(&before, "ward.b"),
        "the edited ward is a NEW fiber"
    );
    assert!(
        until(|| kernel.core().listener_count("ledger/step") == listeners_before).await,
        "a reload left listeners behind: {}",
        kernel.core().listener_count("ledger/step")
    );

    kernel.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_ward_file_takes_its_row_and_its_listener_with_it() {
    let dir = tempfile::tempdir().expect("a temp wards dir");
    std::fs::write(dir.path().join("a.rhai"), WARD_A).unwrap();
    std::fs::write(dir.path().join("b.rhai"), WARD_B).unwrap();
    let kernel = boot(&tree(dir.path())).await;
    assert_eq!(wards(&kernel).len(), 2);
    let before = kernel.core().listener_count("ledger/step");

    std::fs::remove_file(dir.path().join("b.rhai")).unwrap();

    assert!(
        until(|| wards(&kernel).len() == 1).await,
        "the deleted ward's row stayed: {:?}",
        wards(&kernel)
    );
    assert_eq!(wards(&kernel)[0].0, "ward.a");
    assert!(
        until(|| kernel.core().listener_count("ledger/step") == before - 1).await,
        "the deleted ward left its listener behind: {} (was {before})",
        kernel.core().listener_count("ledger/step")
    );
    kernel.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ward_that_does_not_compile_fails_its_own_row_and_leaves_its_sibling_running() {
    let dir = tempfile::tempdir().expect("a temp wards dir");
    std::fs::write(dir.path().join("a.rhai"), WARD_A).unwrap();
    std::fs::write(dir.path().join("bad.rhai"), "fn on_event(ev, cx) { [ }\n").unwrap();
    let kernel = boot(&tree(dir.path())).await;

    let rows = kernel.rows_snapshot();
    fn find(rows: &[RowSnapshot], id: &str) -> Option<RowSnapshot> {
        for r in rows {
            if r.id.as_str() == id {
                return Some(r.clone());
            }
            if let Some(f) = find(&r.children, id) {
                return Some(f);
            }
        }
        None
    }
    let bad = find(&rows, "ward.bad").expect("the bad ward is still a row");
    assert!(
        format!("{:?}", bad.state).to_lowercase().contains("failed"),
        "an uncompilable ward fails its own row: {:?}",
        bad.state
    );
    let good = find(&rows, "ward.a").expect("the good ward is a row");
    assert!(
        format!("{:?}", good.state)
            .to_lowercase()
            .contains("active"),
        "a sibling ward is untouched: {:?}",
        good.state
    );
    kernel.shutdown().await;
}

/// A LIVE firing: the ward is offered a committed step, `evaluate` decides, the executor carries
/// the actions out through the seams, and ONE `ward/fired` step records what happened. This is the
/// other half of "the dry-fire and the live path call the same `evaluate`": the actions the journal
/// records here are exactly the ones `evaluate` returns for that event.
#[tokio::test(flavor = "multi_thread")]
async fn a_live_firing_journals_exactly_what_evaluate_returned() {
    use bough_plugin_ledger::{
        Append, Class, Ledger, LedgerHandle, Order, StepQuery, StepType, TrajId, WakeId,
    };

    const HINTER: &str = r#"
fn triggers() { ["thought/text"] }
fn on_event(ev, cx) {
    [ #{ kind: "hint", agent: "sol", text: "saw: " + ev.body.text } ]
}
"#;
    let dir = tempfile::tempdir().expect("a temp wards dir");
    std::fs::write(dir.path().join("hinter.rhai"), HINTER).unwrap();
    let kernel = boot(&tree(dir.path())).await;

    let ctx = kernel
        .row_context(&bough_kernel::EntryId::new("wards"))
        .expect("the host row's context");
    let ledger = LedgerHandle(
        ctx.get::<Ledger>()
            .expect("the ledger is injected")
            .0
            .clone(),
    );

    let step = ledger
        .0
        .append(Append {
            traj: TrajId::new("t1"),
            wake: WakeId::new("w1"),
            kind: StepType::new("thought/text"),
            class: Class::Thought,
            body: serde_json::json!({ "text": "a thought", "step_index": 0 }),
            cites: vec![],
            at: chrono::Utc::now(),
            id: None,
        })
        .await
        .expect("the step appends");

    let fired = until_some(|| {
        let ledger = ledger.clone();
        async move {
            ledger
                .0
                .steps(&StepQuery {
                    kinds: vec![StepType::new("ward/fired")],
                    order: Order::SeqAsc,
                    ..Default::default()
                })
                .await
                .ok()
                .and_then(|v| v.into_iter().next())
        }
    })
    .await
    .expect("the ward fired");

    let body: bough_plugin_wards_rhai::WardFired =
        serde_json::from_value((*fired.body).clone()).expect("the `ward/fired` body parses");
    assert_eq!(body.ward, "hinter");
    assert_eq!(body.on, step.seq);
    assert_eq!(
        body.actions,
        vec![bough_plugin_runtime_actions::RuntimeAction::Hint {
            agent: "sol".into(),
            text: "saw: a thought".into(),
        }],
        "the journal carries exactly what `evaluate` returned"
    );
    // There is no live `sol` in this tree, so the executor REFUSED the hint — and said so, in the
    // same row. A ward's intent and the boundary's answer are both reconstructible.
    assert_eq!(body.outcomes.len(), 1);
    assert!(
        body.outcomes[0].starts_with("refused:"),
        "{:?}",
        body.outcomes
    );

    // And the ward did not fire on its own journal row: exactly one `ward/fired` exists.
    let all = ledger
        .0
        .steps(&StepQuery {
            kinds: vec![StepType::new("ward/fired")],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(all.len(), 1, "a ward fired on its own firing: {all:?}");
    kernel.shutdown().await;
}

/// Poll an async condition until it yields a value.
async fn until_some<T, F, Fut>(mut f: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    for _ in 0..200 {
        if let Some(v) = f().await {
            return Some(v);
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    None
}
