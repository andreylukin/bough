//! The shared fixture: a headless shell over a root context, a recording pane, and — when a test
//! needs one — a stub agent through a `ledger-memory` tree.
//!
//! Everything is offline and hermetic: no terminal is entered, no clock is read for a decision and
//! no model is called.

#![allow(dead_code)]

use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, KernelCore};
use bough_plugin_agents::{
    Agent, AgentCell, AgentDriver, AgentError, AgentFactory, AgentKind, AgentsHandle, Attach,
    CancelCause, CreateAgent, InboxReceipt, Message, Status, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_commands::{CommandsConfig, CommandsHandle};
use bough_plugin_ledger::{AgentName, LedgerHandle, TrajId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_tui_shell::{
    Pane, PaneCx, PaneEvent, PaneId, PaneOutcome, PaneSpec, RenderCx, Slot, SlotSize, TuiConfig,
    TuiHandle,
};
use parking_lot::Mutex;

/// A shell with no agents and no commands registry: enough for layout, selection and keymap.
pub fn shell() -> (Context, TuiHandle) {
    let ctx = Context::root(KernelCore::new());
    let tui = TuiHandle::new(
        ctx.clone(),
        Arc::new(config()),
        None,
        None,
        /* is_tty */ false,
    )
    .expect("a headless shell needs no terminal");
    (ctx, tui)
}

/// A real commands registry, so the slash path is exercised end to end rather than stubbed.
pub fn registry(ctx: &Context) -> Arc<CommandsHandle> {
    Arc::new(CommandsHandle::new(
        ctx.clone(),
        Arc::new(CommandsConfig {
            prefix: '/',
            suggestions: true,
        }),
    ))
}

/// Register the shell's four built-ins into the registry the shell was built with.
pub async fn register_builtins(ctx: &Context, tui: &TuiHandle) {
    let commands = tui_commands(tui);
    for spec in bough_plugin_tui_shell::builtins::specs(tui) {
        commands
            .register(ctx, spec)
            .await
            .expect("the built-in registers");
    }
}

/// The registry the shell holds. Only a test reaches for it this way.
fn tui_commands(tui: &TuiHandle) -> Arc<CommandsHandle> {
    tui.commands().expect("this fixture wires a registry")
}

/// A shell over a real (in-memory) ledger and agent roster, with a recording driver attached.
pub async fn shell_with_agents() -> (Context, TuiHandle, Arc<AgentsHandle>, Arc<RecordingFactory>) {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    let agents = Arc::new(AgentsHandle::new(ctx.clone(), ledger));
    let factory = Arc::new(RecordingFactory::default());
    agents
        .set_factory(&ctx, factory.clone() as Arc<dyn AgentFactory>)
        .await
        .expect("the factory slot is free");
    let commands = registry(&ctx);
    let tui = TuiHandle::new(
        ctx.clone(),
        Arc::new(config()),
        Some(agents.clone()),
        Some(commands),
        false,
    )
    .expect("a headless shell needs no terminal");
    (ctx, tui, agents, factory)
}

/// Create and focus one agent.
pub async fn focused_agent(tui: &TuiHandle, agents: &AgentsHandle, name: &str) -> Agent {
    let (agent, disposer) = agents
        .create(CreateAgent {
            name: AgentName::new(name),
            traj: TrajId::new(format!("lane/{name}")),
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: chrono::Utc::now(),
        })
        .await
        .expect("the agent is created");
    // The disposer is the roster's, not this test's: dropping it must not tear the agent down.
    std::mem::forget(disposer);
    tui.focus(bough_plugin_tui_shell::FocusRequest {
        agent: Some(agent.id().clone()),
        ..Default::default()
    })
    .await;
    agent
}

pub fn config() -> TuiConfig {
    let mut cfg = bough_plugin_tui_shell::test_config();
    cfg.size = [80, 24];
    cfg
}

/// A pane that records what it was handed and paints one recognisable line.
pub struct Recorder {
    pub label: String,
    pub events: Mutex<Vec<String>>,
    /// What `handle` returns. `Ignored` by default.
    pub outcome: Mutex<PaneOutcome>,
    pub scrolled: Mutex<i32>,
}

impl Recorder {
    pub fn new(label: &str) -> Arc<Recorder> {
        Arc::new(Recorder {
            label: label.to_string(),
            events: Mutex::new(Vec::new()),
            outcome: Mutex::new(PaneOutcome::Ignored),
            scrolled: Mutex::new(0),
        })
    }
    pub fn events(&self) -> Vec<String> {
        self.events.lock().clone()
    }
    pub fn scrolled(&self) -> i32 {
        *self.scrolled.lock()
    }
}

#[async_trait::async_trait]
impl Pane for Recorder {
    fn render(&self, cx: &mut RenderCx<'_>) {
        use ratatui::widgets::Paragraph;
        let area = cx.area;
        let text = format!("{} {}x{}", self.label, area.width, area.height);
        cx.frame.render_widget(Paragraph::new(text), area);
        // One clickable region: the pane's first row.
        cx.hit(
            ratatui::layout::Rect { height: 1, ..area },
            bough_plugin_tui_shell::HitId::new(format!("row:{}", self.label)),
        );
    }

    async fn handle(&self, ev: PaneEvent, _cx: PaneCx) -> PaneOutcome {
        let note = match &ev {
            PaneEvent::Key(k) => format!("key:{:?}", k.code),
            PaneEvent::Click { at, hit, .. } => {
                format!(
                    "click:{:?}:{}",
                    at,
                    hit.as_ref().map(|h| h.to_string()).unwrap_or_default()
                )
            }
            PaneEvent::Scroll { delta } => {
                *self.scrolled.lock() += *delta as i32;
                format!("scroll:{delta}")
            }
            PaneEvent::FocusChanged(f) => format!("focus:{f}"),
            PaneEvent::Focus(_) => "focus-request".to_string(),
            PaneEvent::Tick => "tick".to_string(),
        };
        self.events.lock().push(note);
        self.outcome.lock().clone()
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("↑/↓", "scroll")]
    }
}

/// Register one recorder pane and hand back its disposer.
pub async fn add_pane(
    ctx: &Context,
    tui: &TuiHandle,
    id: &str,
    slot: Slot,
    order: i32,
    size: SlotSize,
) -> (Arc<Recorder>, EffectHandle) {
    let pane = Recorder::new(id);
    let handle = tui
        .register_pane(
            ctx,
            PaneSpec {
                id: PaneId::new(id),
                slot,
                order,
                size,
                title: id.to_string(),
                focusable: slot != Slot::Status,
                pane: pane.clone() as Arc<dyn Pane>,
            },
        )
        .await
        .expect("the pane registers");
    (pane, handle)
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
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        let driver = Arc::new(RecordingDriver {
            calls: Mutex::new(Vec::new()),
            _cell: cell,
        });
        self.attached.lock().push(driver.clone());
        Ok(driver as Arc<dyn AgentDriver>)
    }
}

pub struct RecordingDriver {
    pub calls: Mutex<Vec<DriverCall>>,
    _cell: AgentCell,
}

impl RecordingDriver {
    pub fn calls(&self) -> Vec<DriverCall> {
        self.calls.lock().clone()
    }
    pub fn notifies(&self) -> usize {
        self.calls()
            .into_iter()
            .filter(|c| matches!(c, DriverCall::Notify(_)))
            .count()
    }
}

#[async_trait::async_trait]
impl AgentDriver for RecordingDriver {
    fn driver(&self) -> &'static str {
        "recording-loop"
    }
    async fn wake_now(&self, _kind: WakeKind, _cause: WakeCause) -> WakeRequest {
        self.calls.lock().push(DriverCall::WakeNow);
        WakeRequest::Nothing
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
    }
}

/// The status an agent reports, for a test that wants to read it without importing `Status`.
pub fn is_running(a: &Agent) -> bool {
    a.status() == Status::Running
}
