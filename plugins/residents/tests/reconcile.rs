//! `reconcile_rows` and the open wake: an agent INSIDE a wake is deferred, never bounced.
//! The bug this pins (2026-09-01, the ASI ledger): `merge_lanes` whose survivor was the calling
//! leader moved the leader's own row, and the immediate dispose-and-resume cancelled the
//! leader's program mid-run — the remaining merges in it were silently dropped.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::{
    AgentCell, AgentDriver, AgentError, AgentFactory, AgentsHandle, Attach, CancelCause,
    InboxReceipt, Message, RowsChanged, Status, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_ledger::{AgentName, LedgerHandle};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_residents::{raise_roster, reconcile_rows, ResidentsConfig, Roster};
use parking_lot::Mutex;

/// A driver that does nothing; the test drives status through the CELL, the way a real loop does.
struct InertDriver;

#[async_trait::async_trait]
impl AgentDriver for InertDriver {
    fn driver(&self) -> &'static str {
        "inert-loop"
    }
    async fn notify(&self, _receipt: &InboxReceipt, _msg: &Message) {}
    async fn cancel(&self, _cause: CancelCause, _keep_inbox: bool) {}
    async fn stop(&self) {}
    async fn wake_now(&self, _kind: WakeKind, _cause: WakeCause) -> WakeRequest {
        WakeRequest::Nothing
    }
}

/// Keeps every attached agent's cell, so the test can publish `Running`/`Idle`.
struct CellFactory(Arc<Mutex<Vec<AgentCell>>>);

#[async_trait::async_trait]
impl AgentFactory for CellFactory {
    fn driver(&self) -> &'static str {
        "inert-loop"
    }
    async fn attach(
        &self,
        cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        self.0.lock().push(cell);
        Ok(Arc::new(InertDriver) as Arc<dyn AgentDriver>)
    }
}

#[tokio::test]
async fn a_mid_wake_agent_is_deferred_not_bounced() {
    let core = KernelCore::new();
    let ctx = Context::root(core);
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    let agents = AgentsHandle::new(ctx.clone(), ledger.clone());
    let cells: Arc<Mutex<Vec<AgentCell>>> = Arc::default();
    agents
        .set_factory(
            &ctx,
            Arc::new(CellFactory(Arc::clone(&cells))) as Arc<dyn AgentFactory>,
        )
        .await
        .expect("the slot is free");

    let cfg = ResidentsConfig {
        bootstrap: vec!["trunk".to_string()],
        traj_prefix: "lane/".to_string(),
        resume_all: true,
        catch_up: false,
    };
    let roster = Roster::default();
    raise_roster(&agents, &ledger, &cfg, &roster)
        .await
        .expect("the roster comes up");
    let name = AgentName::new("trunk");
    let cell = cells.lock().pop().expect("the factory saw the agent");

    // The row is deleted UNDER the running agent — what absorbing a mid-wake lane looks like.
    cell.set_status(Status::Running).await.expect("running");
    let changed = RowsChanged {
        written: vec![],
        deleted: vec![name.clone()],
    };
    let (touched, deferred) = reconcile_rows(&agents, &ledger, &roster, &changed)
        .await
        .expect("reconciles");
    assert!(touched.is_empty(), "nothing was bounced: {touched:?}");
    assert_eq!(
        deferred,
        vec![name.clone()],
        "the running agent is deferred"
    );
    assert!(
        agents.by_name(&name).is_some(),
        "the open wake was left to finish"
    );

    // The wake seals; the SAME reconcile, re-run as the deferred half does, now disposes it.
    cell.set_status(Status::Idle).await.expect("idle");
    let (touched, deferred) = reconcile_rows(&agents, &ledger, &roster, &changed)
        .await
        .expect("reconciles");
    assert_eq!(touched, vec![name.clone()], "the sealed agent is bounced");
    assert!(deferred.is_empty());
    assert!(
        agents.by_name(&name).is_none(),
        "the absorbed lane is down once its wake sealed"
    );
}
