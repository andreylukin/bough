//! The shared fixture: a root kernel context, an in-memory ledger and a recording loop driver.
//!
//! The driver records what the SEAM did to it (`notify`, `cancel`, `stop`), so every lifecycle
//! claim in §2 is asserted against an observer outside the crate rather than against its own
//! private state.

#![allow(dead_code)]

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::{
    Agent, AgentCell, AgentDriver, AgentError, AgentFactory, AgentsHandle, Attach, CancelCause,
    InboxReceipt, Message, Sender, Status,
};
use bough_plugin_ledger::{AgentName, LedgerHandle, TrajId};
use bough_plugin_ledger_memory::store::MemoryStore;
use parking_lot::Mutex;

pub struct Fixture {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    pub agents: AgentsHandle,
    pub factory: Arc<RecordingFactory>,
}

pub fn fixture() -> Fixture {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    let agents = AgentsHandle::new(ctx.clone(), ledger.clone());
    Fixture {
        ctx,
        ledger,
        agents,
        factory: Arc::new(RecordingFactory::default()),
    }
}

impl Fixture {
    /// A fixture with the recording factory already in the slot.
    pub async fn mounted() -> Fixture {
        let f = fixture();
        f.agents
            .set_factory(&f.ctx, f.factory.clone() as Arc<dyn AgentFactory>)
            .await
            .expect("the slot is free");
        f
    }

    pub fn traj(&self) -> TrajId {
        TrajId::new("t-1")
    }
}

pub fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

pub fn msg(text: &str) -> Message {
    Message::new(Sender::Andrey, "subject", text, now())
}

pub fn name(n: &str) -> AgentName {
    AgentName::new(n)
}

/// What the seam did to the driver.
#[derive(Clone, Debug, PartialEq)]
pub enum DriverCall {
    Notify(String),
    Cancel(CancelCause, bool),
    Stop,
    WakeNow,
}

#[derive(Default)]
pub struct RecordingFactory {
    pub attached: Mutex<Vec<Arc<RecordingDriver>>>,
    /// Set to make `attach` fail, so the transaction's rollback can be exercised.
    pub refuse: Mutex<bool>,
}

impl RecordingFactory {
    pub fn last(&self) -> Arc<RecordingDriver> {
        self.attached.lock().last().cloned().expect("an attachment")
    }
}

#[async_trait::async_trait]
impl AgentFactory for RecordingFactory {
    fn driver(&self) -> &'static str {
        "recording-loop"
    }
    async fn attach(
        &self,
        cell: AgentCell,
        mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        if *self.refuse.lock() {
            return Err(AgentError::NoFactory);
        }
        let driver = Arc::new(RecordingDriver {
            cell,
            mode,
            calls: Mutex::new(Vec::new()),
            wakes: Mutex::new(Vec::new()),
        });
        self.attached.lock().push(driver.clone());
        Ok(driver as Arc<dyn AgentDriver>)
    }
}

pub struct RecordingDriver {
    pub cell: AgentCell,
    pub mode: Attach,
    pub calls: Mutex<Vec<DriverCall>>,
    /// Every wake `wake_now` opened, so "exactly one" is countable.
    pub wakes: Mutex<Vec<bough_plugin_ledger::WakeId>>,
}

impl RecordingDriver {
    pub fn calls(&self) -> Vec<DriverCall> {
        self.calls.lock().clone()
    }
    pub fn cancels(&self) -> Vec<DriverCall> {
        self.calls()
            .into_iter()
            .filter(|c| matches!(c, DriverCall::Cancel(..)))
            .collect()
    }
    pub fn notifies(&self) -> Vec<DriverCall> {
        self.calls()
            .into_iter()
            .filter(|c| matches!(c, DriverCall::Notify(_)))
            .collect()
    }
    pub fn agent(&self) -> &Agent {
        self.cell.agent()
    }
    /// Enter a wake, the way a real loop would.
    pub async fn run(&self) {
        self.cell
            .set_status(Status::Running)
            .await
            .expect("idle → running");
    }
    pub async fn finish(&self) {
        self.cell
            .set_status(Status::Idle)
            .await
            .expect("running → idle");
    }
}

#[async_trait::async_trait]
impl AgentDriver for RecordingDriver {
    fn driver(&self) -> &'static str {
        "recording-loop"
    }
    /// The documented rule (§2.5), implemented the way a driver must: nothing queued is NOTHING —
    /// no wake, no synthetic message — and anything queued is exactly one wake.
    async fn wake_now(
        &self,
        _kind: bough_plugin_agents::WakeKind,
        _cause: bough_plugin_agents::WakeCause,
    ) -> bough_plugin_agents::WakeRequest {
        self.calls.lock().push(DriverCall::WakeNow);
        if self.cell.agent().inbox().is_empty() {
            return bough_plugin_agents::WakeRequest::Nothing;
        }
        let wake = bough_plugin_ledger::WakeId::new(uuid::Uuid::now_v7().to_string());
        self.cell.wake_started();
        self.wakes.lock().push(wake.clone());
        bough_plugin_agents::WakeRequest::Started(wake)
    }
    async fn notify(&self, receipt: &InboxReceipt, _msg: &Message) {
        self.calls
            .lock()
            .push(DriverCall::Notify(receipt.message.to_string()));
    }
    async fn cancel(&self, cause: CancelCause, keep_inbox: bool) {
        self.calls
            .lock()
            .push(DriverCall::Cancel(cause, keep_inbox));
    }
    async fn stop(&self) {
        self.calls.lock().push(DriverCall::Stop);
        // The teardown trace's first entry, recorded by the DRIVER: if the seam's own "stop"
        // label were a lie, this would not come first.
        bough_plugin_agents::trace::push(self.agent().id(), "driver.stop");
    }
}

/// A second factory, so "the slot is taken" and "the slot was freed" are distinguishable.
pub struct OtherFactory;

#[async_trait::async_trait]
impl AgentFactory for OtherFactory {
    fn driver(&self) -> &'static str {
        "other-loop"
    }
    async fn attach(
        &self,
        _cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        Err(AgentError::NoFactory)
    }
}
