//! Invariant under test: a ward is PURE and SANDBOXED. `evaluate` returns actions and touches no
//! seam, the engine cannot spell I/O, and the dry-fire path and the live path call the SAME
//! function — so `bough wards test` can neither act nor drift.

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{AgentName, Ref, Seq, StepType, TrajId, WakeId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_runtime_actions::{RuntimeAction, RuntimeLimits};
use bough_plugin_wards_rhai::{
    dry_run, engine::build_engine, evaluate, parse_since, render_dry_run, CompiledWard, DryRun,
    Since, WardError, WardEvent, WardHostConfig, WardView,
};
use chrono::{TimeZone, Utc};

fn host() -> WardHostConfig {
    WardHostConfig {
        dir: std::path::PathBuf::from("/nonexistent"),
        glob: "*.rhai".into(),
        watch: false,
        debounce_ms: 50,
        max_ops: 100_000,
        max_depth: 16,
        max_string_bytes: 10_000,
        max_array_size: 100,
        eval_timeout_ms: 1_000,
        limits: RuntimeLimits {
            max_actions: 8,
            max_spawns: 1,
            max_text_bytes: 4_000,
        },
    }
}

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/fixtures/wards")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn event(kind: &str, seq: u64, body: serde_json::Value) -> WardEvent {
    WardEvent {
        kind: StepType::new(kind),
        seq: Seq(seq),
        traj: TrajId::new("t1"),
        agent: Some(AgentName::new("sol")),
        wake: WakeId::new("w1"),
        at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        body,
        cites: vec![],
        refs: vec![Ref::new("gh:o/r#12")],
    }
}

fn view(events: &[WardEvent]) -> WardView {
    WardView {
        ward: "reviews".into(),
        agent_names: vec!["sol".into(), "terra".into()],
        now_ms: 1_767_225_600_000,
        recent: events.to_vec(),
        acted: vec![Ref::new("gh:o/r#9")],
    }
}

// ---------------------------------------------------------------------------
// the sandbox
// ---------------------------------------------------------------------------

#[test]
fn eval_is_disabled() {
    let e = build_engine(&host());
    let err = e
        .compile(r#"fn on_event(ev, cx) { eval("1") }"#)
        .expect_err("`eval` is not spellable");
    assert!(
        err.to_string().to_lowercase().contains("eval"),
        "the error names it: {err}"
    );
}

#[test]
fn a_file_cannot_be_opened() {
    let e = build_engine(&host());
    let ward = CompiledWard::compile(
        "probe",
        r#"fn on_event(ev, cx) { open_file("/etc/passwd") }"#,
        &e,
    )
    .expect("it compiles; there is simply no such function");
    let ev = event("thought/text", 1, serde_json::json!({}));
    let err = evaluate(&ward, &ev, &view(&[]), &e).expect_err("no file function exists");
    assert!(
        matches!(&err, WardError::Runtime { detail, .. } if detail.contains("open_file")),
        "{err}"
    );
}

#[test]
fn the_environment_cannot_be_read() {
    let e = build_engine(&host());
    let ward =
        CompiledWard::compile("probe", r#"fn on_event(ev, cx) { env("HOME") }"#, &e).unwrap();
    let ev = event("thought/text", 1, serde_json::json!({}));
    assert!(evaluate(&ward, &ev, &view(&[]), &e).is_err());
}

#[test]
fn a_network_call_is_not_spellable() {
    let e = build_engine(&host());
    for probe in [
        r#"fn on_event(ev, cx) { http_get("http://example.com") }"#,
        r#"fn on_event(ev, cx) { import "std"; [] }"#,
    ] {
        let compiled = CompiledWard::compile("probe", probe, &e);
        let failed = match compiled {
            Err(_) => true,
            Ok(w) => evaluate(
                &w,
                &event("thought/text", 1, serde_json::json!({})),
                &view(&[]),
                &e,
            )
            .is_err(),
        };
        assert!(failed, "a ward reached the network with: {probe}");
    }
}

#[test]
fn a_runaway_script_is_terminated_and_named() {
    let mut cfg = host();
    cfg.max_ops = 2_000;
    let e = build_engine(&cfg);
    let ward = CompiledWard::compile("runaway", &fixture("runaway.rhai"), &e).unwrap();
    let err = evaluate(
        &ward,
        &event("thought/text", 1, serde_json::json!({})),
        &view(&[]),
        &e,
    )
    .expect_err("it never returns on its own");
    assert!(
        matches!(&err, WardError::TooManyOps { ward, .. } if ward == "runaway"),
        "{err}"
    );
    // REPORTED, NOT RETRIED: the next firing is an ordinary call, and the engine is unharmed.
    let quiet = CompiledWard::compile("quiet", &fixture("quiet.rhai"), &e).unwrap();
    assert_eq!(
        evaluate(
            &quiet,
            &event("thought/text", 2, serde_json::json!({})),
            &view(&[]),
            &e
        )
        .unwrap(),
        Vec::<RuntimeAction>::new()
    );
}

#[test]
fn an_over_deep_script_is_refused_by_the_depth_limit() {
    let mut cfg = host();
    cfg.max_depth = bough_plugin_wards_rhai::MAX_DEPTH_FLOOR;
    let e = build_engine(&cfg);
    let deep = format!(
        "fn on_event(ev, cx) {{ let x = {}1{}; [] }}",
        "(".repeat(40),
        ")".repeat(40)
    );
    let err = match CompiledWard::compile("deep", &deep, &e) {
        Err(e) => e,
        Ok(_) => panic!("40 levels of nesting exceeds the depth limit"),
    };
    assert!(matches!(err, WardError::TooDeep { .. }), "{err}");
}

#[test]
fn a_ward_with_no_on_event_is_refused_by_name() {
    let e = build_engine(&host());
    let err = match CompiledWard::compile("empty", "fn triggers() { [] }", &e) {
        Err(e) => e,
        Ok(_) => panic!("a ward with no `on_event` is not a ward"),
    };
    assert!(
        matches!(&err, WardError::NoEntryPoint { ward } if ward == "empty"),
        "{err}"
    );
}

#[test]
fn a_ward_that_returns_something_else_is_a_named_failure() {
    let e = build_engine(&host());
    let ward = CompiledWard::compile("odd", r#"fn on_event(ev, cx) { 42 }"#, &e).unwrap();
    let err = evaluate(
        &ward,
        &event("thought/text", 1, serde_json::json!({})),
        &view(&[]),
        &e,
    )
    .unwrap_err();
    assert!(
        matches!(&err, WardError::BadReturn { ward, .. } if ward == "odd"),
        "{err}"
    );
}

// ---------------------------------------------------------------------------
// purity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn evaluate_is_pure_and_touches_no_seam() {
    let e = build_engine(&host());
    let ward = CompiledWard::compile("reviews", &fixture("reviews.rhai"), &e).unwrap();
    let ev = event(
        "mail/delivered",
        7,
        serde_json::json!({ "subject": "review please" }),
    );

    // Every seam the executor could reach, live and observable.
    let ctx = Context::root(KernelCore::new());
    let ledger = bough_plugin_ledger::LedgerHandle(MemoryStore::new(ctx.clone()));
    let actions = bough_plugin_actions::ActionsHandle::new(ledger.clone());

    let first = evaluate(&ward, &ev, &view(&[]), &e).unwrap();
    let second = evaluate(&ward, &ev, &view(&[]), &e).unwrap();
    assert_eq!(first, second, "the same event twice is the same list");
    assert_eq!(first.len(), 1);

    // Nothing was written anywhere: evaluation is the whole of a ward, and it is data in, data out.
    let steps = ledger
        .0
        .steps(&bough_plugin_ledger::StepQuery::default())
        .await
        .unwrap();
    assert!(steps.is_empty(), "evaluate wrote to the ledger: {steps:?}");
    assert!(
        actions.pending().await.unwrap().is_empty(),
        "evaluate reached the actions seam"
    );
}

#[test]
fn triggers_narrow_what_a_ward_sees() {
    let e = build_engine(&host());
    let ward = CompiledWard::compile("reviews", &fixture("reviews.rhai"), &e).unwrap();
    assert_eq!(ward.triggers, vec![StepType::new("mail/delivered")]);
    assert!(ward.wants(&StepType::new("mail/delivered")));
    assert!(!ward.wants(&StepType::new("thought/text")));

    // No `triggers()` at all ⇒ every step type.
    let quiet = CompiledWard::compile("quiet", &fixture("quiet.rhai"), &e).unwrap();
    assert!(quiet.triggers.is_empty());
    assert!(quiet.wants(&StepType::new("anything/at-all")));
}

#[test]
fn the_dry_fire_and_the_live_path_call_the_same_evaluate() {
    let e = build_engine(&host());
    let ward = CompiledWard::compile("reviews", &fixture("reviews.rhai"), &e).unwrap();
    let events = vec![
        event("mail/delivered", 1, serde_json::json!({ "subject": "one" })),
        event("thought/text", 2, serde_json::json!({ "text": "ignored" })),
        event("mail/delivered", 3, serde_json::json!({ "subject": "two" })),
    ];
    let v = view(&events);

    // The dry-fire path.
    let d = dry_run(&ward, &events, &v, &e);

    // The live path's own call, event by event — the SAME function the host's listener calls.
    let live: Vec<(Seq, Vec<RuntimeAction>)> = events
        .iter()
        .filter(|ev| ward.wants(&ev.kind))
        .filter_map(|ev| {
            let a = evaluate(&ward, ev, &v, &e).unwrap();
            (!a.is_empty()).then_some((ev.seq, a))
        })
        .collect();

    assert_eq!(d.fired, live);
    assert_eq!(
        d.considered, 2,
        "the thought was never offered to this ward"
    );
}

#[test]
fn the_dry_run_rendering_is_stable() {
    let d = DryRun {
        ward: "reviews".into(),
        fired: vec![(
            Seq(7),
            vec![RuntimeAction::Hint {
                agent: "sol".into(),
                text: "a review request landed: review please".into(),
            }],
        )],
        errors: vec![(Seq(9), "ward `reviews`: boom".into())],
        considered: 3,
    };
    assert_eq!(
        render_dry_run(&d),
        "ward `reviews`: 3 events considered, 1 would fire, 1 errors\n\
         \x20 seq 7:\n\
         \x20   would hint `sol`: a review request landed: review please\n\
         \x20 seq 9: ERROR ward `reviews`: boom\n"
    );
}

#[test]
fn since_parses_a_seq_and_a_duration_and_refuses_the_rest() {
    assert_eq!(parse_since("1234"), Some(Since::Seq(1234)));
    assert_eq!(
        parse_since("24h"),
        Some(Since::Ago(chrono::Duration::hours(24)))
    );
    assert_eq!(parse_since("last tuesday"), None);
    assert_eq!(parse_since(""), None);
}
