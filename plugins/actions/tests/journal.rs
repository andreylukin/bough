//! V7 — the actions journal (§7). Every case here is about ONE sentence: an act on the world is
//! journalled before it happens and concluded after, and the idem key is what makes a second
//! attempt at the same act collide instead of duplicating it.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_actions::{
    idem_key, marker_for, ActionArtifact, ActionError, ActionKind, ActionProvider, ActionRequest,
    ActionTarget, ActionsHandle, ExecuteRequest,
};
use bough_plugin_ledger::{
    ActionQuery, ActionStatus, AgentName, AgentRow, LedgerHandle, NewAction, StepId, StepQuery,
    TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use chrono::{DateTime, TimeZone, Utc};
use parking_lot::Mutex;

const AGENT: &str = "sol";
const TRAJ: &str = "t1";

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap()
}

/// A mounted memory ledger with one agent row, and the actions seam over it.
async fn fixture() -> (Context, LedgerHandle, ActionsHandle) {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    ledger
        .0
        .put_agent(AgentRow {
            name: AgentName::new(AGENT),
            traj: TrajId::new(TRAJ),
            routing_refs: Default::default(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("the agent row goes in");
    let actions = ActionsHandle::new(ledger.clone());
    (ctx, ledger, actions)
}

fn request(kind: ActionKind, target: &str, step: &str) -> ActionRequest {
    ActionRequest {
        kind,
        target: ActionTarget::new(target),
        payload: serde_json::json!({ "title": "a title" }),
        agent: AgentName::new(AGENT),
        wake: WakeId::new("w1"),
        step: StepId::new(step),
        at: at(),
    }
}

/// A Provider that records what it saw, and answers however the test told it to.
struct Spy {
    kinds: Vec<ActionKind>,
    seen: Arc<Mutex<Vec<Seen>>>,
    fail: bool,
    /// Read at execute time, so a case can assert on the journal DURING the act.
    ledger: LedgerHandle,
}

#[derive(Clone, Debug)]
struct Seen {
    marker: String,
    idem: String,
    canonical: String,
    /// The status of this action's own row while the Provider was running.
    status_during: Option<ActionStatus>,
}

#[async_trait::async_trait]
impl ActionProvider for Spy {
    fn kinds(&self) -> Vec<ActionKind> {
        self.kinds.clone()
    }
    async fn execute(&self, req: &ExecuteRequest) -> Result<ActionArtifact, ActionError> {
        let rows = self
            .ledger
            .0
            .actions(&ActionQuery::default())
            .await
            .expect("the journal reads");
        self.seen.lock().push(Seen {
            marker: req.marker.clone(),
            idem: req.idem_key.as_str().to_string(),
            canonical: req.canonical_target.clone(),
            status_during: rows.iter().find(|r| r.id == req.action).map(|r| r.status),
        });
        if self.fail {
            return Err(ActionError::Provider {
                kind: "open_pr",
                source: anyhow::anyhow!("the remote said no"),
            });
        }
        Ok(ActionArtifact {
            locator: "https://github.com/owner/repo/pull/99".into(),
            marker: req.marker.clone(),
            detail: serde_json::json!({}),
        })
    }
}

async fn spy(
    ctx: &Context,
    ledger: &LedgerHandle,
    actions: &ActionsHandle,
    fail: bool,
) -> Arc<Mutex<Vec<Seen>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    actions
        .provider(
            ctx,
            Arc::new(Spy {
                kinds: ActionKind::all().to_vec(),
                seen: seen.clone(),
                fail,
                ledger: ledger.clone(),
            }),
        )
        .await
        .expect("the provider registers");
    seen
}

async fn step_kinds(ledger: &LedgerHandle) -> Vec<String> {
    ledger
        .0
        .steps(&StepQuery {
            trajs: vec![TrajId::new(TRAJ)],
            ..Default::default()
        })
        .await
        .expect("the steps read")
        .iter()
        .map(|s| s.kind.as_str().to_string())
        .collect()
}

#[tokio::test]
async fn intent_is_written_before_execute_and_done_after() {
    let (ctx, ledger, actions) = fixture().await;
    let seen = spy(&ctx, &ledger, &actions, false).await;

    let artifact = actions
        .execute(&ctx, request(ActionKind::OpenPr, "owner/repo", "s1"))
        .await
        .expect("the action goes through");
    assert_eq!(artifact.locator, "https://github.com/owner/repo/pull/99");

    // DURING the provider call, the row already existed and was `intent`.
    let seen = seen.lock().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].status_during, Some(ActionStatus::Intent));

    // AFTER, it is done — and both steps are in the ledger, in that order.
    let rows = ledger
        .0
        .actions(&ActionQuery::default())
        .await
        .expect("the journal reads");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, ActionStatus::Done);
    assert!(rows[0].done_at.is_some());

    let kinds = step_kinds(&ledger).await;
    let intent = kinds.iter().position(|k| k == "action/intent");
    let done = kinds.iter().position(|k| k == "action/done");
    assert!(
        intent.is_some() && done.is_some() && intent < done,
        "expected action/intent before action/done, saw {kinds:?}"
    );
}

#[tokio::test]
async fn the_same_kind_target_and_step_collide_instead_of_duplicating() {
    let (ctx, ledger, actions) = fixture().await;
    let seen = spy(&ctx, &ledger, &actions, false).await;

    actions
        .execute(&ctx, request(ActionKind::OpenPr, "owner/repo", "s1"))
        .await
        .expect("the first goes through");
    let again = actions
        .execute(&ctx, request(ActionKind::OpenPr, "owner/repo", "s1"))
        .await
        .expect_err("the second collides");
    assert!(
        matches!(
            again,
            ActionError::Duplicate {
                kind: "open_pr",
                ..
            }
        ),
        "expected a Duplicate, got {again}"
    );

    // The Provider ran exactly once, and there is exactly one row.
    assert_eq!(seen.lock().len(), 1);
    assert_eq!(
        ledger
            .0
            .actions(&ActionQuery::default())
            .await
            .expect("the journal reads")
            .len(),
        1
    );
}

#[tokio::test]
async fn two_spellings_of_one_target_produce_one_idem_key() {
    // The pure formula, first: the canonicaliser is what collapses them.
    let a = ActionTarget::new("https://GitHub.com/Owner/Repo/pull/12")
        .canonical(ActionKind::PushToPr)
        .unwrap();
    let b = ActionTarget::new("owner/repo#12")
        .canonical(ActionKind::PushToPr)
        .unwrap();
    let step = StepId::new("s1");
    assert_eq!(
        idem_key(ActionKind::PushToPr, &a, &step),
        idem_key(ActionKind::PushToPr, &b, &step)
    );

    // And through the executor: the second spelling collides in the journal.
    let (ctx, _ledger, actions) = fixture().await;
    let _ = spy(&ctx, &_ledger, &actions, false).await;
    actions
        .execute(
            &ctx,
            request(
                ActionKind::PushToPr,
                "https://GitHub.com/Owner/Repo/pull/12",
                "s1",
            ),
        )
        .await
        .expect("the first goes through");
    let again = actions
        .execute(&ctx, request(ActionKind::PushToPr, "owner/repo#12", "s1"))
        .await
        .expect_err("the other spelling of the same act collides");
    assert!(matches!(again, ActionError::Duplicate { .. }));
}

#[tokio::test]
async fn an_unregistered_kind_is_refused_by_the_executor() {
    let (ctx, ledger, actions) = fixture().await;
    // A Provider that claims ONLY `open_pr`: every other kind does not exist.
    let seen = Arc::new(Mutex::new(Vec::new()));
    actions
        .provider(
            &ctx,
            Arc::new(Spy {
                kinds: vec![ActionKind::OpenPr],
                seen: seen.clone(),
                fail: false,
                ledger: ledger.clone(),
            }),
        )
        .await
        .expect("the provider registers");
    assert_eq!(actions.kinds(), vec![ActionKind::OpenPr]);

    let e = actions
        .execute(&ctx, request(ActionKind::LinearWrite, "TEAM-123", "s1"))
        .await
        .expect_err("no provider claims linear_write");
    assert!(matches!(e, ActionError::NoProvider("linear_write")));
    assert!(e.to_string().contains("linear_write"));

    // A refused kind journals NOTHING: nothing was attempted on the world.
    assert!(ledger
        .0
        .actions(&ActionQuery::default())
        .await
        .expect("the journal reads")
        .is_empty());
    assert!(seen.lock().is_empty());
}

#[tokio::test]
async fn reconciliation_lists_intent_without_done_without_re_executing() {
    let (ctx, ledger, actions) = fixture().await;
    let seen = spy(&ctx, &ledger, &actions, false).await;

    // A crash between the two writes leaves exactly this: a row with an intent and no done.
    let canonical = "owner/repo".to_string();
    let idem = idem_key(ActionKind::OpenPr, &canonical, &StepId::new("s-crashed"));
    ledger
        .0
        .action_intent(NewAction {
            id: None,
            traj: TrajId::new(TRAJ),
            wake: WakeId::new("w0"),
            target: canonical.clone(),
            idem_key: idem.clone(),
            kind: "open_pr".into(),
            payload: serde_json::json!({ "target": canonical }),
            at: at(),
        })
        .await
        .expect("the intent row goes in");

    // A concluded action alongside it, so `pending` is a filter and not "everything".
    actions
        .execute(&ctx, request(ActionKind::OpenPr, "other/repo", "s2"))
        .await
        .expect("the concluded one goes through");

    let pending = actions.pending().await.expect("reconciliation lists");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, ActionKind::OpenPr);
    assert_eq!(pending[0].target, "owner/repo");
    assert_eq!(pending[0].idem_key, idem);
    // The marker is recomputable from the row alone: reconciliation is a lookup, never a re-run.
    assert_eq!(pending[0].marker, marker_for(&idem));

    // And listing did not act: the Provider ran once, for the OTHER action.
    assert_eq!(seen.lock().len(), 1);
    assert_eq!(seen.lock()[0].canonical, "other/repo");
}

#[tokio::test]
async fn the_marker_the_provider_is_handed_is_derived_from_the_idem_key() {
    let (ctx, ledger, actions) = fixture().await;
    let seen = spy(&ctx, &ledger, &actions, false).await;
    actions
        .execute(&ctx, request(ActionKind::OpenPr, "owner/repo", "s1"))
        .await
        .expect("the action goes through");

    let seen = seen.lock().clone();
    let expected = idem_key(ActionKind::OpenPr, "owner/repo", &StepId::new("s1"));
    assert_eq!(seen[0].idem, expected.as_str());
    assert_eq!(seen[0].marker, marker_for(&expected));
    assert!(seen[0].marker.contains(&expected.as_str()[..16]));
}

#[tokio::test]
async fn a_provider_failure_marks_the_row_failed_and_still_writes_a_done() {
    let (ctx, ledger, actions) = fixture().await;
    let _seen = spy(&ctx, &ledger, &actions, true).await;

    let e = actions
        .execute(&ctx, request(ActionKind::OpenPr, "owner/repo", "s1"))
        .await
        .expect_err("the provider failed");
    assert!(e.to_string().contains("the remote said no"));

    let rows = ledger
        .0
        .actions(&ActionQuery::default())
        .await
        .expect("the journal reads");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, ActionStatus::Failed);
    assert!(rows[0].done_at.is_some());
    assert!(step_kinds(&ledger)
        .await
        .contains(&"action/done".to_string()));

    // A failed act is CONCLUDED, so reconciliation must not list it as unfinished work.
    assert!(actions.pending().await.expect("lists").is_empty());
    assert_eq!(
        bough_plugin_actions::invariant::evaluate(&rows),
        Ok(()),
        "a failed row still satisfies intent-before-done"
    );
}
