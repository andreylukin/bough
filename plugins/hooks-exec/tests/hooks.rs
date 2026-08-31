//! §2.11: JSON on stdin, JSON on stdout, the returned actions journaled through the boundary, and
//! ONE failure mode with four spellings — non-zero exit, timeout, unparseable stdout, oversized
//! stdout — counted the same and quarantining the POINT after `max_failures`.
//!
//! Every assertion here is on a real subprocess. The recording [`Recorder`] stands in for
//! `runtime_actions::execute_all`, so "the actions are journaled" is asserted on what reached the
//! boundary rather than on the hook's exit code.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use bough_plugin_hooks_exec::{
    ActionSink, HookInput, HookPoint, HookState, HooksConfig, HooksHost,
};
use bough_plugin_runtime_actions::{
    ActionOutcome, RuntimeAction, RuntimeLimits, RuntimeSource, Trigger,
};
use chrono::{DateTime, TimeZone, Utc};

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/fixtures/hooks")
        .join(name)
        .canonicalize()
        .unwrap_or_else(|e| panic!("fixture `{name}`: {e}"))
}

fn point(name: &str, exec: &str, record: Option<&std::path::Path>) -> HookPoint {
    let mut env = BTreeMap::new();
    if let Some(p) = record {
        env.insert("HOOK_RECORD".to_string(), p.display().to_string());
    }
    HookPoint {
        point: name.into(),
        exec: fixture(exec),
        args: vec![],
        timeout_ms: 2000,
        env,
    }
}

fn cfg(points: Vec<HookPoint>) -> Arc<HooksConfig> {
    Arc::new(HooksConfig {
        points,
        max_output_bytes: 65536,
        max_failures: 3,
        limits: RuntimeLimits {
            max_actions: 16,
            max_spawns: 2,
            max_text_bytes: 8192,
        },
    })
}

/// A sink that records what reached the boundary and performs nothing.
#[derive(Default)]
struct Recorder {
    seen: parking_lot::Mutex<Vec<(RuntimeSource, Vec<RuntimeAction>)>>,
}

#[async_trait::async_trait]
impl ActionSink for Recorder {
    async fn execute(
        &self,
        source: &RuntimeSource,
        _trigger: &Trigger,
        actions: &[RuntimeAction],
        _at: DateTime<Utc>,
    ) -> Vec<ActionOutcome> {
        self.seen.lock().push((source.clone(), actions.to_vec()));
        actions
            .iter()
            .map(|_| ActionOutcome::Did {
                detail: "recorded".into(),
            })
            .collect()
    }
}

fn event() -> serde_json::Value {
    serde_json::json!({ "step": "s1", "kind": "mail/delivered" })
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_hook_receives_the_documented_json_on_stdin_and_its_actions_are_journaled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let record = dir.path().join("stdin.jsonl");
    let host = HooksHost::new(cfg(vec![point(
        "mail/delivered",
        "echo-input.sh",
        Some(&record),
    )]));
    let sink = Recorder::default();

    let fired = host
        .fire(
            "mail/delivered",
            event(),
            at(),
            &Trigger::synthetic(&RuntimeSource::Hook("mail/delivered".into())),
            &sink,
        )
        .await;

    // stdin carried exactly `HookInput`.
    let written = std::fs::read_to_string(&record).expect("the hook recorded its stdin");
    let input: HookInput = serde_json::from_str(written.trim()).expect("stdin was one HookInput");
    assert_eq!(input.point, "mail/delivered");
    assert_eq!(input.at, at().to_rfc3339());
    assert_eq!(input.event, event());

    // The actions reached the boundary, under this point's name.
    let seen = sink.seen.lock().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, RuntimeSource::Hook("mail/delivered".into()));
    assert_eq!(
        seen[0].1,
        vec![RuntimeAction::Hint {
            agent: "sol".into(),
            text: "a hook said so".into()
        }]
    );

    // And the row the host would append says so.
    assert_eq!(fired.len(), 1);
    assert!(fired[0].ok);
    assert_eq!(fired[0].outcomes, vec!["did: recorded".to_string()]);
    assert_eq!(host.exec_count("mail/delivered"), 1);
    assert_eq!(host.hooks()[0].2, HookState::Ready);
}

#[tokio::test]
async fn a_hook_that_returns_nothing_is_not_a_failure() {
    let host = HooksHost::new(cfg(vec![point("boot", "silent.sh", None)]));
    let sink = Recorder::default();
    let fired = host
        .fire(
            "boot",
            event(),
            at(),
            &Trigger::synthetic(&RuntimeSource::Hook("boot".into())),
            &sink,
        )
        .await;
    assert!(fired[0].ok, "empty stdout is the empty output");
    assert!(fired[0].actions.is_empty());
    assert_eq!(host.hooks()[0].2, HookState::Ready);
}

#[tokio::test]
async fn a_hook_exiting_non_zero_is_reported_and_counted() {
    let host = HooksHost::new(cfg(vec![point("boot", "fails.sh", None)]));
    let sink = Recorder::default();
    let fired = host
        .fire(
            "boot",
            event(),
            at(),
            &Trigger::synthetic(&RuntimeSource::Hook("boot".into())),
            &sink,
        )
        .await;
    assert!(!fired[0].ok);
    assert!(fired[0].actions.is_empty());
    assert!(sink.seen.lock().is_empty(), "nothing reached the boundary");
    match &host.hooks()[0].2 {
        HookState::Failing { consecutive, last } => {
            assert_eq!(*consecutive, 1);
            assert!(
                last.contains("exited 3"),
                "the report names the exit: {last}"
            );
        }
        other => panic!("expected one counted failure, got {other:?}"),
    }
}

/// The four spellings of ONE failure. Each is counted, each leaves the point `Failing` after one
/// dispatch, and none of them reaches the boundary.
#[tokio::test]
async fn unparseable_oversized_and_timed_out_are_all_the_same_failure() {
    for (exec, why) in [
        ("garbage.sh", "unparseable stdout"),
        ("flood.sh", "oversized stdout"),
        ("sleeps.sh", "a timeout"),
    ] {
        let mut p = point("boot", exec, None);
        p.timeout_ms = 150;
        let host = HooksHost::new(Arc::new(HooksConfig {
            points: vec![p],
            max_output_bytes: 4096,
            max_failures: 3,
            limits: RuntimeLimits {
                max_actions: 16,
                max_spawns: 2,
                max_text_bytes: 8192,
            },
        }));
        let sink = Recorder::default();
        let fired = host
            .fire(
                "boot",
                event(),
                at(),
                &Trigger::synthetic(&RuntimeSource::Hook("boot".into())),
                &sink,
            )
            .await;
        assert!(!fired[0].ok, "{why} must be a failure");
        assert!(sink.seen.lock().is_empty(), "{why} must reach no boundary");
        assert!(
            matches!(host.hooks()[0].2, HookState::Failing { consecutive: 1, .. }),
            "{why} must be COUNTED like any other: {:?}",
            host.hooks()[0].2
        );
    }
}

#[tokio::test]
async fn max_failures_consecutive_failures_quarantine_the_point_and_it_is_not_invoked_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let record = dir.path().join("runs");
    let host = HooksHost::new(cfg(vec![point("boot", "fails.sh", Some(&record))]));
    let sink = Recorder::default();
    let trigger = Trigger::synthetic(&RuntimeSource::Hook("boot".into()));

    for _ in 0..3 {
        host.fire("boot", event(), at(), &trigger, &sink).await;
    }
    match &host.hooks()[0].2 {
        HookState::Quarantined { reason } => {
            assert!(reason.contains("3 consecutive failures"), "{reason}");
        }
        other => panic!("expected a quarantine after max_failures, got {other:?}"),
    }
    assert_eq!(host.exec_count("boot"), 3);

    // Six more dispatches. The counter — and the fixture's own record — must not move.
    for _ in 0..6 {
        host.fire("boot", event(), at(), &trigger, &sink).await;
    }
    assert_eq!(
        host.exec_count("boot"),
        3,
        "a quarantined point is NOT invoked again"
    );
    let runs = std::fs::read_to_string(&record).expect("the fixture recorded its runs");
    assert_eq!(
        runs.lines().count(),
        3,
        "the executable itself ran exactly three times"
    );
    assert!(sink.seen.lock().is_empty());
}

#[tokio::test]
async fn one_success_clears_the_failure_streak() {
    let dir = tempfile::tempdir().expect("tempdir");
    let good = point("boot", "echo-input.sh", Some(&dir.path().join("in")));
    let host = HooksHost::new(cfg(vec![point("boot", "fails.sh", None), good]));
    let sink = Recorder::default();
    let trigger = Trigger::synthetic(&RuntimeSource::Hook("boot".into()));

    // Two dispatches: the failing hook is at 2, the good one is Ready and has journaled twice.
    host.fire("boot", event(), at(), &trigger, &sink).await;
    host.fire("boot", event(), at(), &trigger, &sink).await;
    let hooks = host.hooks();
    assert!(matches!(
        hooks[0].2,
        HookState::Failing { consecutive: 2, .. }
    ));
    assert_eq!(hooks[1].2, HookState::Ready, "its sibling is untouched");
    assert_eq!(sink.seen.lock().len(), 2);
}

#[tokio::test]
async fn a_point_nobody_configured_dispatches_nothing() {
    let host = HooksHost::new(cfg(vec![point("boot", "echo-input.sh", None)]));
    let sink = Recorder::default();
    let fired = host
        .fire(
            "mail/delivered",
            event(),
            at(),
            &Trigger::synthetic(&RuntimeSource::Hook("mail/delivered".into())),
            &sink,
        )
        .await;
    assert!(fired.is_empty());
    assert_eq!(host.exec_count("boot"), 0);
}

#[tokio::test]
async fn dispatch_returns_what_the_hook_returned_without_executing_it() {
    let host = HooksHost::new(cfg(vec![point("boot", "echo-input.sh", None)]));
    let actions = host.dispatch("boot", event(), at()).await;
    assert_eq!(
        actions,
        vec![RuntimeAction::Hint {
            agent: "sol".into(),
            text: "a hook said so".into()
        }]
    );
}

/// `validate` refuses a point that is not spellable as one. Before this it checked only that the
/// name was non-empty, so `power/changed` (unwired at the time) and every typo mounted green.
#[test]
fn a_point_that_is_not_shaped_like_a_point_is_refused_at_load() {
    use bough_plugin_hooks_exec::is_point_shaped;
    for good in ["boot", "schedule/fired", "power/changed", "mail/delivered"] {
        assert!(is_point_shaped(good), "{good}");
    }
    for bad in [
        "",
        " ",
        "boot ",
        "mail",
        "mail/",
        "/delivered",
        "a/b/c",
        "mail delivered",
    ] {
        assert!(!is_point_shaped(bad), "{bad:?} is not a point");
    }
}
