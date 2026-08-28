//! V6 — crash reconciliation, exercised through the REAL GitHub Provider.
//!
//! `plugins/actions-reconcile/tests/reconcile.rs` drives the reconciler against a hand-written
//! Provider, which can only prove the reconciler's own branching. This file instead registers the
//! real `GithubActions` — the same type that would have executed the action — on the real seam,
//! over a recording `gh` that answers from a scripted GitHub API shape. So the marker match is the
//! production one (`gh api repos/…/pulls` → `/body` contains the marker), and "never re-executed"
//! is asserted on the recorded argv log.

mod support;

use std::sync::Arc;

use bough_plugin_actions::{idem_key, marker_for, ActionKind, ActionProvider, ActionsHandle};
use bough_plugin_actions_reconcile::{ReconcileConfig, Reconciler};
use bough_plugin_drafts::DraftsHandle;
use bough_plugin_ledger::{
    ActionQuery, ActionStatus, IdemKey, LedgerHandle, NewAction, Order, Step, StepId, StepQuery,
    StepType, TrajId, WakeId,
};
use support::{at, fixture, provider, FakeGh, AGENT, TRAJ};

/// The drafts a pass wrote, read back out of the ledger.
async fn drafted(ledger: &LedgerHandle) -> Vec<Step> {
    ledger
        .0
        .steps(&StepQuery {
            trajs: vec![TrajId::new(TRAJ)],
            kinds: vec![StepType::new("draft/message")],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("a read")
}

/// What a crash between the journal's two writes leaves behind: one `open_pr` intent, no done.
async fn crashed(actions: &ActionsHandle, ledger: &LedgerHandle) -> IdemKey {
    let idem = idem_key(ActionKind::OpenPr, "owner/repo", &StepId::new("s1"));
    ledger
        .0
        .action_intent(NewAction {
            id: None,
            traj: TrajId::new(TRAJ),
            wake: WakeId::new("w1"),
            target: "owner/repo".into(),
            idem_key: idem.clone(),
            kind: "open_pr".into(),
            payload: serde_json::json!({ "target": "owner/repo" }),
            at: at(),
        })
        .await
        .expect("the intent row goes in");
    assert_eq!(
        actions.pending().await.expect("pending reads").len(),
        1,
        "the crash left exactly one intent-without-done"
    );
    idem
}

/// The real Provider on the real seam, and a reconciler over both. The registration handle is
/// returned so the effect outlives the pass.
async fn reconcile_over(
    ledger: &LedgerHandle,
    actions: &ActionsHandle,
    gh: &Arc<FakeGh>,
) -> (Reconciler, bough_kernel::EffectHandle) {
    let ctx = bough_kernel::Context::root(bough_kernel::KernelCore::new());
    let reg = actions
        .provider(&ctx, provider(gh) as Arc<dyn ActionProvider>)
        .await
        .expect("the real Provider registers");
    (
        Reconciler::new(
            Arc::new(ReconcileConfig {
                at_boot: true,
                surface_to: AGENT.into(),
            }),
            actions.clone(),
            DraftsHandle::new(ledger.clone(), 50),
        ),
        reg,
    )
}

/// A PR whose body carries the marker: the world says the act HAPPENED, so the row concludes with
/// the located artifact and nothing is written anywhere.
#[tokio::test]
async fn the_real_provider_finds_the_marker_in_the_world_and_the_row_is_marked_done() {
    let (_ctx, ledger, actions) = fixture().await;
    let idem = crashed(&actions, &ledger).await;
    let marker = marker_for(&idem);

    let gh = Arc::new(FakeGh::new("andrey").read(
        "repos/owner/repo/pulls",
        serde_json::json!([
            { "html_url": "https://github.com/owner/repo/pull/12", "body": "unrelated PR" },
            { "html_url": "https://github.com/owner/repo/pull/99",
              "body": format!("did the thing\n\n{marker}\n") },
        ]),
    ));
    let (r, _reg) = reconcile_over(&ledger, &actions, &gh).await;

    let report = r.reconcile(at()).await.expect("the pass runs");
    assert_eq!(report.marked_done.len(), 1, "report: {report:?}");
    assert!(report.surfaced.is_empty() && report.unknown_kind.is_empty());

    let row = &ledger.0.actions(&ActionQuery::default()).await.unwrap()[0];
    assert_eq!(row.status, ActionStatus::Done);
    let result = row.result.clone().expect("the row carries its artifact");
    assert_eq!(
        result["locator"], "https://github.com/owner/repo/pull/99",
        "the PR the MARKER was in, not the first PR listed"
    );
    assert_eq!(result["marker"], marker);
    assert_eq!(result["reconciled"], true);

    assert!(
        drafted(&ledger).await.is_empty(),
        "nothing to tell anyone about"
    );
    // The whole point: the real transport saw READS only.
    let log = gh.log();
    assert!(
        log.iter().all(|c| !c.write),
        "reconciliation must never call a write path; the gh log was {log:?}"
    );
    assert_eq!(
        log.iter().map(|c| c.argv.clone()).collect::<Vec<_>>(),
        vec![vec![
            "api".to_string(),
            "repos/owner/repo/pulls?state=all&per_page=100".to_string()
        ]],
        "exactly one read of the PR list, and no `gh pr create`"
    );
}

/// The same world, minus the marker: the act did NOT happen. It is not retried — Andrey is told,
/// and the row is left open.
#[tokio::test]
async fn the_real_provider_finds_no_marker_so_it_is_surfaced_as_a_draft_and_never_re_executed() {
    let (_ctx, ledger, actions) = fixture().await;
    let idem = crashed(&actions, &ledger).await;

    let gh = Arc::new(FakeGh::new("andrey").read(
        "repos/owner/repo/pulls",
        serde_json::json!([
            { "html_url": "https://github.com/owner/repo/pull/12", "body": "somebody else's PR" },
        ]),
    ));
    let (r, _reg) = reconcile_over(&ledger, &actions, &gh).await;

    let report = r.reconcile(at()).await.expect("the pass runs");
    assert_eq!(report.surfaced.len(), 1, "report: {report:?}");
    assert!(report.marked_done.is_empty());

    let row = &ledger.0.actions(&ActionQuery::default()).await.unwrap()[0];
    assert_eq!(
        row.status,
        ActionStatus::Intent,
        "left open: a person decides, and it is never re-executed"
    );
    assert!(row.result.is_none());

    let rows = drafted(&ledger).await;
    assert_eq!(rows.len(), 1);
    let body = rows[0].body.to_string();
    assert!(body.contains("unfinished open_pr"), "{body}");
    assert!(body.contains("owner/repo"), "{body}");
    assert!(body.contains(&marker_for(&idem)), "{body}");
    assert!(body.contains("Nothing was re-executed"), "{body}");

    let log = gh.log();
    assert!(
        log.iter().all(|c| !c.write),
        "the absent marker must not trigger a re-execution; the gh log was {log:?}"
    );
    assert_eq!(log.len(), 1, "one read, then it stopped: {log:?}");
}

/// Reconciling twice over an unfinished intent is still two reads and zero writes: the pass is
/// idempotent because it is a lookup.
#[tokio::test]
async fn a_second_pass_over_the_same_open_intent_still_writes_nothing() {
    let (_ctx, ledger, actions) = fixture().await;
    crashed(&actions, &ledger).await;
    let gh = Arc::new(FakeGh::new("andrey").read(
        "repos/owner/repo/pulls",
        serde_json::json!([{ "html_url": "u", "body": "no marker here" }]),
    ));
    let (r, _reg) = reconcile_over(&ledger, &actions, &gh).await;

    r.reconcile(at()).await.expect("pass one");
    r.reconcile(at()).await.expect("pass two");

    let log = gh.log();
    assert_eq!(log.len(), 2, "two reads: {log:?}");
    assert!(log.iter().all(|c| !c.write));
    assert_eq!(
        ledger.0.actions(&ActionQuery::default()).await.unwrap()[0].status,
        ActionStatus::Intent
    );
}
