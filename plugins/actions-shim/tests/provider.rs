//! WP-4: the two Provider-level facts V3 leans on — one execute is one `gh` invocation, and a
//! failing `gh` still closes the journal row rather than leaving an intent with no done.
//!
//! The shim is a recording script written into a temp dir; the real `gh` is never called
//! (AGENTS.md).

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_actions::{
    idem_key, marker_for, ActionKind, ActionProvider, ActionRequest, ActionTarget, ActionsHandle,
    ExecuteRequest,
};
use bough_plugin_actions_shim::{invariant, GhShimProvider, ShimConfig};
use bough_plugin_ledger::{
    ActionId, ActionQuery, ActionStatus, AgentName, AgentRow, LedgerHandle, StepId, StepQuery,
    TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use chrono::{DateTime, TimeZone, Utc};

const AGENT: &str = "sol";

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap()
}

/// A recording shim: every invocation appends its argv to `log`. `ok` decides its exit status.
fn shim(dir: &std::path::Path, ok: bool) -> (String, std::path::PathBuf) {
    let log = dir.join("invocations.log");
    let path = dir.join("gh-shim");
    let body = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n{tail}\n",
        log = log.display(),
        tail = if ok {
            "echo https://github.com/owner/repo/pull/99\nexit 0"
        } else {
            "echo 'the remote said no' >&2\nexit 3"
        }
    );
    std::fs::write(&path, body).expect("the shim is written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("the shim is executable");
    }
    (path.display().to_string(), log)
}

fn lines(log: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn cfg(gh: &str) -> Arc<ShimConfig> {
    Arc::new(ShimConfig {
        gh: gh.to_string(),
        kinds: ActionKind::all().to_vec(),
        // The two windows exist; a test that is not killing the process wants them at zero.
        delay_before_ms: 0,
        delay_after_ms: 0,
    })
}

#[tokio::test]
async fn one_execute_is_one_gh_invocation() {
    // No `forget()` here: the invocation record is PROCESS-global on purpose, and the other test
    // in this binary runs concurrently. Each test uses its own step, so each owns its own key.
    let dir = tempfile::tempdir().expect("a temp dir");
    let (gh, log) = shim(dir.path(), true);

    let step = StepId::new("s-one-execute");
    let idem = idem_key(ActionKind::OpenPr, "owner/repo", &step);
    let marker = marker_for(&idem);
    let req = ExecuteRequest {
        request: Arc::new(ActionRequest {
            kind: ActionKind::OpenPr,
            target: ActionTarget::new("owner/repo"),
            payload: serde_json::json!({}),
            agent: AgentName::new(AGENT),
            wake: WakeId::new("w1"),
            step,
            at: at(),
        }),
        action: ActionId::new("a1"),
        idem_key: idem.clone(),
        marker: marker.clone(),
        canonical_target: "owner/repo".into(),
    };

    let artifact = GhShimProvider::new(cfg(&gh))
        .execute(&req)
        .await
        .expect("the shim succeeds");

    // ONE act on the world, carrying the journal's own marker.
    let invocations = lines(&log);
    assert_eq!(
        invocations.len(),
        1,
        "one execute, one invocation: {invocations:?}"
    );
    assert!(
        invocations[0].contains(&marker),
        "the marker is embedded in the artifact; got {}",
        invocations[0]
    );
    assert_eq!(artifact.marker, marker);
    assert_eq!(artifact.locator, "https://github.com/owner/repo/pull/99");

    // And the invariant agrees: this idem key was acted on exactly once.
    let counts = invariant::invocations();
    assert_eq!(
        counts.iter().find(|(k, _)| *k == idem).map(|(_, n)| *n),
        Some(1),
        "one act per key; got {counts:?}"
    );
    assert_eq!(invariant::check_counts(&counts), Ok(()));
}

#[tokio::test]
async fn a_failing_gh_marks_the_row_failed_and_still_writes_action_done() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let (gh, log) = shim(dir.path(), false);

    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    ledger
        .0
        .put_agent(AgentRow {
            name: AgentName::new(AGENT),
            traj: TrajId::new("t1"),
            routing_refs: BTreeSet::new(),
            wake_classes: BTreeSet::new(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("the agent row goes in");

    let actions = ActionsHandle::new(ledger.clone());
    actions
        .provider(&ctx, Arc::new(GhShimProvider::new(cfg(&gh))))
        .await
        .expect("the provider registers");

    let err = actions
        .execute(
            &ctx,
            ActionRequest {
                kind: ActionKind::OpenPr,
                target: ActionTarget::new("owner/repo"),
                payload: serde_json::json!({ "title": "a title" }),
                agent: AgentName::new(AGENT),
                wake: WakeId::new("w1"),
                step: StepId::new("s-failing-gh"),
                at: at(),
            },
        )
        .await
        .expect_err("a non-zero exit is a provider failure");
    assert!(
        err.to_string().contains("exited 3"),
        "the failure says what the shim did; got {err}"
    );

    // The act was attempted exactly once, and it was CONCLUDED: a failed row is not
    // unreconciled work (§7).
    assert_eq!(lines(&log).len(), 1);
    let rows = actions
        .ledger()
        .0
        .actions(&ActionQuery::default())
        .await
        .expect("the journal reads");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, ActionStatus::Failed);
    assert!(rows[0].done_at.is_some(), "the row was concluded");

    // And the `action/done` step is in the ledger next to its intent.
    let steps = ledger
        .0
        .steps(&StepQuery::default())
        .await
        .expect("the ledger reads");
    let kinds: Vec<&str> = steps.iter().map(|s| s.kind.as_str()).collect();
    assert!(
        kinds.contains(&"action/intent") && kinds.contains(&"action/done"),
        "intent before done, both durable; got {kinds:?}"
    );

    // Nothing is left for reconciliation to re-execute.
    assert!(actions.pending().await.expect("pending reads").is_empty());
}
