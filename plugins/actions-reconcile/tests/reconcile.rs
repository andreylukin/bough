//! §7 — a crash between the journal's two writes is repaired by LOOKING, never by acting again.
//! The marker is in the world ⇒ the row concludes. It is not ⇒ a person is told and the row stays
//! open. Neither path touches a write.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_actions::{
    idem_key, marker_for, ActionArtifact, ActionError, ActionKind, ActionsHandle,
};
use bough_plugin_actions_reconcile::{
    ArtifactLookup, Drafting, LookupRegistry, ReconcileConfig, Reconciler,
};
use bough_plugin_drafts::{DraftError, NewDraft};
use bough_plugin_ledger::{
    ActionQuery, ActionStatus, AgentName, AgentRow, IdemKey, LedgerHandle, NewAction, StepId,
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

/// Everything a lookup was asked, and whether anything ever asked it to WRITE. There is no write
/// method: the absence is the point, so the log records the reads and the test asserts the shape.
#[derive(Default)]
struct FakeLookup {
    reads: Mutex<Vec<(String, String)>>,
    found: Option<ActionArtifact>,
}

#[async_trait::async_trait]
impl ArtifactLookup for FakeLookup {
    fn kinds(&self) -> Vec<ActionKind> {
        vec![ActionKind::OpenPr]
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

/// A recording draft surface.
#[derive(Default)]
struct FakeDrafts(Mutex<Vec<NewDraft>>);

#[async_trait::async_trait]
impl Drafting for FakeDrafts {
    async fn draft(&self, d: NewDraft) -> Result<(), DraftError> {
        self.0.lock().push(d);
        Ok(())
    }
}

/// A ledger with one agent and one INTENT-WITHOUT-DONE row: what a crash leaves behind.
async fn crashed() -> (LedgerHandle, ActionsHandle, IdemKey) {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx) as Arc<_>);
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
    (ledger, actions, idem)
}

fn reconciler(
    actions: &ActionsHandle,
    lookup: Arc<FakeLookup>,
    drafts: Arc<FakeDrafts>,
) -> (Reconciler, LookupRegistry) {
    let registry = LookupRegistry::new();
    // The bare registry API a mounted row reaches through `ctx`; here it is used directly so the
    // pass can be tested without a tree.
    registry_push(&registry, lookup);
    (
        Reconciler::new(
            Arc::new(ReconcileConfig {
                at_boot: true,
                surface_to: AGENT.into(),
            }),
            actions.clone(),
            registry.clone(),
            drafts,
        ),
        registry,
    )
}

/// Registration without a `Context`: a root context, so the effect has somewhere to hang.
fn registry_push(registry: &LookupRegistry, lookup: Arc<FakeLookup>) {
    let ctx = Context::root(KernelCore::new());
    futures::executor::block_on(async {
        registry
            .register(&ctx, lookup as Arc<dyn ArtifactLookup>)
            .await
            .expect("the lookup registers");
    });
}

async fn status(ledger: &LedgerHandle) -> ActionStatus {
    ledger
        .0
        .actions(&ActionQuery::default())
        .await
        .expect("the journal reads")[0]
        .status
}

#[tokio::test]
async fn an_intent_whose_marker_is_in_the_world_is_marked_done() {
    let (ledger, actions, idem) = crashed().await;
    let lookup = Arc::new(FakeLookup {
        reads: Default::default(),
        found: Some(ActionArtifact {
            locator: "https://github.com/owner/repo/pull/99".into(),
            marker: marker_for(&idem),
            detail: serde_json::json!({}),
        }),
    });
    let drafts = Arc::new(FakeDrafts::default());
    let (r, _reg) = reconciler(&actions, lookup.clone(), drafts.clone());

    let report = r.reconcile(at()).await.expect("the pass runs");
    assert_eq!(report.marked_done.len(), 1);
    assert!(report.surfaced.is_empty());
    assert_eq!(status(&ledger).await, ActionStatus::Done);

    let row = &ledger.0.actions(&ActionQuery::default()).await.unwrap()[0];
    let result = row.result.clone().expect("the row carries its artifact");
    assert_eq!(result["locator"], "https://github.com/owner/repo/pull/99");
    assert_eq!(result["reconciled"], true);
    assert_eq!(
        lookup.reads.lock()[0],
        ("owner/repo".to_string(), marker_for(&idem))
    );
    assert!(drafts.0.lock().is_empty(), "nothing to tell anyone about");
}

#[tokio::test]
async fn an_intent_whose_marker_is_absent_is_surfaced_as_a_draft_and_left_intent() {
    let (ledger, actions, _idem) = crashed().await;
    let lookup = Arc::new(FakeLookup::default());
    let drafts = Arc::new(FakeDrafts::default());
    let (r, _reg) = reconciler(&actions, lookup, drafts.clone());

    let report = r.reconcile(at()).await.expect("the pass runs");
    assert_eq!(report.surfaced.len(), 1);
    assert!(report.marked_done.is_empty());
    assert_eq!(
        status(&ledger).await,
        ActionStatus::Intent,
        "the row is LEFT open: a person decides, and it is never re-executed"
    );
    let drafted = drafts.0.lock().clone();
    assert_eq!(drafted.len(), 1);
    assert_eq!(drafted[0].agent, AgentName::new(AGENT));
    assert!(drafted[0].subject.contains("unfinished open_pr"));
    assert!(drafted[0].body.contains("owner/repo"));
    assert!(drafted[0].body.contains("Nothing was re-executed"));
}

/// A kind no lookup claims is REPORTED, not guessed at: the row stays open and nothing is drafted
/// as if it had been examined.
#[tokio::test]
async fn a_pending_row_no_lookup_claims_is_reported() {
    let (ledger, actions, _idem) = crashed().await;
    let drafts = Arc::new(FakeDrafts::default());
    let r = Reconciler::new(
        Arc::new(ReconcileConfig {
            at_boot: true,
            surface_to: AGENT.into(),
        }),
        actions,
        LookupRegistry::new(),
        drafts.clone(),
    );
    let report = r.reconcile(at()).await.expect("the pass runs");
    assert_eq!(report.unknown_kind.len(), 1);
    assert!(report.marked_done.is_empty() && report.surfaced.is_empty());
    assert_eq!(status(&ledger).await, ActionStatus::Intent);
    assert!(drafts.0.lock().is_empty());
}

/// The whole point, stated as one assertion: the ONLY thing reconciliation asked the world was a
/// read, and the row's own journal is the only thing it changed.
#[tokio::test]
async fn reconciliation_never_calls_a_write_path() {
    let (_ledger, actions, idem) = crashed().await;
    let lookup = Arc::new(FakeLookup {
        reads: Default::default(),
        found: Some(ActionArtifact {
            locator: "x".into(),
            marker: marker_for(&idem),
            detail: serde_json::json!({}),
        }),
    });
    let drafts = Arc::new(FakeDrafts::default());
    let (r, _reg) = reconciler(&actions, lookup.clone(), drafts);
    r.reconcile(at()).await.expect("the pass runs");
    assert_eq!(
        lookup.reads.lock().len(),
        1,
        "exactly one read per pending row, and no other call exists on the trait"
    );
}

/// Registration is an EFFECT: a lookup leaves with its row.
#[tokio::test]
async fn a_lookup_leaves_the_registry_with_its_registration() {
    let ctx = Context::root(KernelCore::new());
    let registry = LookupRegistry::new();
    assert!(registry.is_empty());
    let handle = registry
        .register(
            &ctx,
            Arc::new(FakeLookup::default()) as Arc<dyn ArtifactLookup>,
        )
        .await
        .expect("it registers");
    assert!(registry.for_kind(ActionKind::OpenPr).is_some());
    handle.dispose().await;
    assert!(registry.is_empty(), "unload leaves no trace");
    assert!(registry.for_kind(ActionKind::OpenPr).is_none());
}
