//! V11, on the SHIPPED tree: a hook point configured on a real row runs a REAL executable when a
//! real ledger step of that kind lands, the executable gets the documented JSON on stdin, and the
//! actions it returns come back as one `hook/fired` row in the ledger citing the step that caused
//! it. And a hook that fails is reported in that same row and — after `max_failures` — stops being
//! invoked at all: the executable's own on-disk run log stops growing while steps keep arriving.
//!
//! `plugins/hooks-exec/tests/hooks.rs` asserts the host's behaviour against a recording sink; this
//! file asserts the same behaviour end-to-end through `bough::compose` + the kernel + the real
//! sqlite ledger, so "journaled through the plugin API" is read back off the journal.

mod support;

use std::path::PathBuf;

use bough_plugin_hello::trace;
use bough_plugin_ledger::{
    Append, Class, Ledger, LedgerHandle, Step, StepQuery, StepType, TrajId, WakeId,
};
use support::{boot_real, row_ctx};

fn fixture(name: &str) -> PathBuf {
    support::repo_root()
        .join("scripts/fixtures/hooks")
        .join(name)
        .canonicalize()
        .expect("the hook fixture exists")
}

/// A `--patch` file that puts one point on the shipped `hooks` row.
fn patch(tag: &str, exec: &str, record: &std::path::Path, max_failures: u32) -> PathBuf {
    let yaml = format!(
        "entries:\n  hooks:\n    config:\n      points:\n        - point: mail/delivered\n          \
         exec: {exec}\n          args: []\n          timeout_ms: 4000\n          env:\n            \
         HOOK_RECORD: {record}\n      max_output_bytes: 65536\n      max_failures: {max_failures}\n      \
         limits: {{ max_actions: 16, max_spawns: 2, max_text_bytes: 8192 }}\n",
        exec = fixture(exec).display(),
        record = record.display(),
    );
    let path = std::env::temp_dir().join(format!("bough-v11-{tag}-{}.yml", std::process::id()));
    std::fs::write(&path, yaml).expect("the patch file is writable");
    path
}

fn ledger(kernel: &bough_kernel::Kernel) -> LedgerHandle {
    LedgerHandle(
        row_ctx(kernel, "exec")
            .get::<Ledger>()
            .expect("the ledger key is bound")
            .0
            .clone(),
    )
}

/// Append one `mail/delivered` step — the kind the patched hook point is subscribed to.
async fn deliver(l: &LedgerHandle, wake: &str, n: u32) -> Step {
    l.0.append(Append {
        traj: TrajId::new("v11"),
        wake: WakeId::new(wake),
        kind: StepType::new("mail/delivered"),
        class: Class::Evidence,
        body: serde_json::json!({
            "class": "ordinary",
            "from": "collector:github",
            "subject": format!("note {n}"),
            "summary": "a delivery the hook point is subscribed to",
        }),
        cites: vec![bough_plugin_ledger::Cite {
            r#ref: bough_plugin_ledger::Ref::new(format!("gh:pr:{n}")),
            url: None,
        }],
        at: chrono::Utc::now(),
        id: None,
    })
    .await
    .expect("the mail step appends")
}

async fn fired_rows(l: &LedgerHandle) -> Vec<Step> {
    l.0.steps(&StepQuery {
        trajs: vec![],
        kinds: vec![StepType::new("hook/fired")],
        class: None,
        wake: None,
        after: None,
        before: None,
        refs: vec![],
        order: bough_plugin_ledger::Order::SeqAsc,
        limit: None,
    })
    .await
    .expect("the query runs")
}

/// Poll until `n` `hook/fired` rows exist, or fail. The dispatch is a listener on the ledger's own
/// event, so it lands shortly after the append rather than within it.
async fn wait_for_fired(l: &LedgerHandle, n: usize) -> Vec<Step> {
    for _ in 0..200 {
        let rows = fired_rows(l).await;
        if rows.len() >= n {
            return rows;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!(
        "only {:?} `hook/fired` rows after 5s",
        fired_rows(l).await.len()
    );
}

fn lines(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

#[tokio::test]
async fn a_hook_point_on_the_shipped_row_runs_and_its_actions_are_journaled() {
    let _guard = trace::test_lock();
    let record = std::env::temp_dir().join(format!("bough-v11-in-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&record);
    let p = patch("ok", "echo-input.sh", &record, 3);
    let (kernel, _dir) = boot_real("headless", std::slice::from_ref(&p)).await;

    let l = ledger(&kernel);
    let step = deliver(&l, "w1", 1).await;
    let rows = wait_for_fired(&l, 1).await;

    // The executable really ran, and stdin carried the documented JSON.
    let written = std::fs::read_to_string(&record).expect("the hook recorded its stdin");
    let input: serde_json::Value =
        serde_json::from_str(written.trim()).expect("stdin was one JSON object");
    assert_eq!(input["point"], "mail/delivered");
    assert_eq!(input["event"]["step"], step.id.as_str());
    assert_eq!(input["event"]["kind"], "mail/delivered");
    assert_eq!(input["event"]["body"]["subject"], "note 1");

    // ONE `hook/fired` row, carrying the action the executable returned and the outcome the
    // boundary gave it, and citing the step that caused the dispatch.
    assert_eq!(rows.len(), 1, "{rows:#?}");
    let body = &rows[0].body;
    assert_eq!(body["point"], "mail/delivered");
    assert_eq!(body["ok"], true, "{body:#?}");
    assert_eq!(body["actions"][0]["kind"], "hint");
    assert_eq!(body["actions"][0]["text"], "a hook said so");
    assert_eq!(
        body["outcomes"].as_array().map(|a| a.len()),
        Some(1),
        "the returned action reached the boundary and its outcome came back: {body:#?}"
    );
    assert!(
        rows[0]
            .refs
            .iter()
            .any(|r| r.as_str() == format!("step:{}", step.id.as_str())),
        "the row cites the step that fired it: {:?}",
        rows[0].refs
    );

    kernel.shutdown().await;
    let _ = std::fs::remove_file(&p);
}

#[tokio::test]
async fn a_failing_hook_is_reported_and_quarantined_rather_than_retried_into_a_loop() {
    let _guard = trace::test_lock();
    let record = std::env::temp_dir().join(format!("bough-v11-fail-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&record);
    let p = patch("fail", "fails.sh", &record, 2);
    let (kernel, _dir) = boot_real("headless", std::slice::from_ref(&p)).await;

    let l = ledger(&kernel);
    // Two deliveries reach max_failures.
    deliver(&l, "w1", 1).await;
    deliver(&l, "w1", 2).await;
    let rows = wait_for_fired(&l, 2).await;
    assert_eq!(rows.len(), 2, "{rows:#?}");
    for r in &rows {
        assert_eq!(r.body["ok"], false, "{:#?}", r.body);
        assert!(
            r.body["outcomes"]
                .as_array()
                .map(|a| a.is_empty())
                .unwrap_or(false),
            "a failing hook journals no outcome: {:#?}",
            r.body
        );
        assert!(
            r.body["actions"]
                .as_array()
                .map(|a| a.is_empty())
                .unwrap_or(false),
            "nothing reached the boundary: {:#?}",
            r.body
        );
    }
    assert_eq!(lines(&record), 2, "the executable ran once per delivery");

    // Eight more deliveries. The point is quarantined: no new `hook/fired` row, and — the honest
    // half — the executable itself is never launched again.
    for n in 3..11 {
        deliver(&l, "w1", n).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    assert_eq!(
        lines(&record),
        2,
        "a quarantined point must NOT be invoked again"
    );
    // The quarantined point still REPORTS on each dispatch (one `hook/fired` per delivery, ok
    // false) — that is by design in `HooksHost::fire` — but it never invokes anything and never
    // reaches the boundary again.
    let after = fired_rows(&l).await;
    assert_eq!(after.len(), 10, "one report per dispatch: {}", after.len());
    for r in &after {
        assert_eq!(r.body["ok"], false);
        assert!(r.body["actions"].as_array().is_some_and(|a| a.is_empty()));
        assert!(r.body["outcomes"].as_array().is_some_and(|a| a.is_empty()));
    }

    kernel.shutdown().await;
    let _ = std::fs::remove_file(&p);
}
