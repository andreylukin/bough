//! §7 — a crash between the journal's two writes is repaired by LOOKING, never by acting again.
//! The marker is in the world ⇒ the row concludes. It is not ⇒ a person is told and the row stays
//! open. Neither path touches a write.
//!
//! MERGE (note 2): the lookup is `ActionProvider::find_marker` now, so the fake here is a REAL
//! Provider registered on the real `actions` seam — and its `execute` panics, which is what turns
//! "reconciliation never calls a write path" from a comment into a fact the test would catch.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_actions::{
    idem_key, marker_for, ActionArtifact, ActionError, ActionKind, ActionProvider, ActionsHandle,
    ExecuteRequest,
};
use bough_plugin_actions_reconcile::{ReconcileConfig, Reconciler};
use bough_plugin_drafts::DraftsHandle;
use bough_plugin_ledger::{
    ActionQuery, ActionStatus, AgentName, AgentRow, IdemKey, LedgerHandle, NewAction, Order, Step,
    StepId, StepQuery, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use chrono::{DateTime, TimeZone, Utc};
use parking_lot::Mutex;

const AGENT: &str = "sol";
const TRAJ: &str = "t1";

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap()
}

/// Everything the Provider was asked to LOOK UP. `execute` panics: a reconciliation pass that
/// reached a write path would fail this test rather than pass it quietly.
#[derive(Default)]
struct FakeProvider {
    reads: Mutex<Vec<(String, String)>>,
    found: Option<ActionArtifact>,
}

#[async_trait::async_trait]
impl ActionProvider for FakeProvider {
    fn kinds(&self) -> Vec<ActionKind> {
        vec![ActionKind::OpenPr]
    }
    async fn execute(&self, _req: &ExecuteRequest) -> Result<ActionArtifact, ActionError> {
        panic!("reconciliation must never reach a write path");
    }
    async fn find_marker(
        &self,
        _kind: ActionKind,
        canonical_target: &str,
        marker: &str,
    ) -> Result<Option<ActionArtifact>, ActionError> {
        self.reads
            .lock()
            .push((canonical_target.to_string(), marker.to_string()));
        Ok(self.found.clone())
    }
}

/// A ledger with one agent and one INTENT-WITHOUT-DONE row: what a crash leaves behind.
async fn crashed() -> (LedgerHandle, ActionsHandle, DraftsHandle, IdemKey) {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx) as Arc<_>);
    for def in bough_plugin_drafts::step_types() {
        ledger.0.register_step_type(def).expect("a fresh type");
    }
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
            payload: serde_json::json!({
                "target": "owner/repo",
                "marker": marker_for(&idem),
            }),
            at: at(),
        })
        .await
        .expect("the intent row goes in");
    let actions = ActionsHandle::new(ledger.clone());
    let drafts = DraftsHandle::new(ledger.clone(), 50);
    (ledger, actions, drafts, idem)
}

/// Register the fake Provider on the real seam, the way a Provider row does.
async fn register(actions: &ActionsHandle, p: Arc<FakeProvider>) -> bough_kernel::EffectHandle {
    let ctx = Context::root(KernelCore::new());
    actions
        .provider(&ctx, p as Arc<dyn ActionProvider>)
        .await
        .expect("the provider registers")
}

fn reconciler(actions: &ActionsHandle, drafts: &DraftsHandle) -> Reconciler {
    Reconciler::new(
        Arc::new(ReconcileConfig {
            at_boot: true,
            surface_to: AGENT.into(),
        }),
        actions.clone(),
        drafts.clone(),
    )
}

async fn status(ledger: &LedgerHandle) -> ActionStatus {
    ledger
        .0
        .actions(&ActionQuery::default())
        .await
        .expect("the journal reads")[0]
        .status
}

/// The drafts this pass actually wrote, read back out of the ledger — the durable fact, not a
/// trait call somebody counted.
async fn drafted(ledger: &LedgerHandle) -> Vec<Step> {
    ledger
        .0
        .steps(&StepQuery {
            trajs: vec![TrajId::new(TRAJ)],
            kinds: vec![bough_plugin_ledger::StepType::new("draft/message")],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("a read")
}

#[tokio::test]
async fn an_intent_whose_marker_is_in_the_world_is_marked_done() {
    let (ledger, actions, drafts, idem) = crashed().await;
    let provider = Arc::new(FakeProvider {
        reads: Default::default(),
        found: Some(ActionArtifact {
            locator: "https://github.com/owner/repo/pull/99".into(),
            marker: marker_for(&idem),
            detail: serde_json::json!({}),
        }),
    });
    let _reg = register(&actions, provider.clone()).await;

    let report = reconciler(&actions, &drafts)
        .reconcile(at())
        .await
        .expect("the pass runs");
    assert_eq!(report.marked_done.len(), 1);
    assert!(report.surfaced.is_empty());
    assert_eq!(status(&ledger).await, ActionStatus::Done);

    let row = &ledger.0.actions(&ActionQuery::default()).await.unwrap()[0];
    let result = row.result.clone().expect("the row carries its artifact");
    assert_eq!(result["locator"], "https://github.com/owner/repo/pull/99");
    assert_eq!(result["reconciled"], true);
    assert_eq!(
        provider.reads.lock()[0],
        ("owner/repo".to_string(), marker_for(&idem))
    );
    assert!(
        drafted(&ledger).await.is_empty(),
        "nothing to tell anyone about"
    );
}

#[tokio::test]
async fn an_intent_whose_marker_is_absent_is_surfaced_as_a_draft_and_left_intent() {
    let (ledger, actions, drafts, _idem) = crashed().await;
    let _reg = register(&actions, Arc::new(FakeProvider::default())).await;

    let report = reconciler(&actions, &drafts)
        .reconcile(at())
        .await
        .expect("the pass runs");
    assert_eq!(report.surfaced.len(), 1);
    assert!(report.marked_done.is_empty());
    assert_eq!(
        status(&ledger).await,
        ActionStatus::Intent,
        "the row is LEFT open: a person decides, and it is never re-executed"
    );
    let rows = drafted(&ledger).await;
    assert_eq!(rows.len(), 1);
    let body = rows[0].body.to_string();
    assert!(body.contains("unfinished open_pr"), "{body}");
    assert!(body.contains("owner/repo"), "{body}");
    assert!(body.contains("Nothing was re-executed"), "{body}");
}

/// A kind no Provider claims is REPORTED, not guessed at: the row stays open and nothing is
/// drafted as if it had been examined.
#[tokio::test]
async fn a_pending_row_no_provider_claims_is_reported() {
    let (ledger, actions, drafts, _idem) = crashed().await;
    let report = reconciler(&actions, &drafts)
        .reconcile(at())
        .await
        .expect("the pass runs");
    assert_eq!(report.unknown_kind.len(), 1);
    assert!(report.marked_done.is_empty() && report.surfaced.is_empty());
    assert_eq!(status(&ledger).await, ActionStatus::Intent);
    assert!(drafted(&ledger).await.is_empty());
}

/// The whole point, stated as one assertion: the ONLY thing reconciliation asked the world was a
/// read. `FakeProvider::execute` panics, so a pass that reached the write path would fail here.
#[tokio::test]
async fn reconciliation_never_calls_a_write_path() {
    let (_ledger, actions, drafts, idem) = crashed().await;
    let provider = Arc::new(FakeProvider {
        reads: Default::default(),
        found: Some(ActionArtifact {
            locator: "x".into(),
            marker: marker_for(&idem),
            detail: serde_json::json!({}),
        }),
    });
    let _reg = register(&actions, provider.clone()).await;
    reconciler(&actions, &drafts)
        .reconcile(at())
        .await
        .expect("the pass runs");
    assert_eq!(
        provider.reads.lock().len(),
        1,
        "exactly one read per pending row, and `execute` was never reached"
    );
}

/// Registration is an EFFECT: a Provider's lookup leaves with its row, and the kind stops
/// existing — which is the ONE registry now, not two (merge note 2).
#[tokio::test]
async fn a_provider_leaves_the_seam_with_its_registration() {
    let (_ledger, actions, _drafts, _idem) = crashed().await;
    assert!(actions.kinds().is_empty());
    let reg = register(&actions, Arc::new(FakeProvider::default())).await;
    assert_eq!(actions.kinds(), vec![ActionKind::OpenPr]);
    assert!(actions
        .find_marker(ActionKind::OpenPr, "owner/repo", "bough-action:0")
        .await
        .is_ok());
    reg.dispose().await;
    assert!(actions.kinds().is_empty(), "unload leaves no trace");
    assert!(matches!(
        actions
            .find_marker(ActionKind::OpenPr, "owner/repo", "bough-action:0")
            .await,
        Err(ActionError::NoProvider("open_pr"))
    ));
}
