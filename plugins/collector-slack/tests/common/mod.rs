//! The shared fixture: a kernel context, a memory ledger, live agents behind a recording driver,
//! and a STUB Slack MCP server on the mcp seam. Nothing in this test binary can reach a real
//! Slack: the row's wire is the seam and the seam holds only the stub.

#![allow(dead_code)]

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::{
    Agent, AgentCell, AgentDriver, AgentError, AgentFactory, AgentsHandle, Attach, CancelCause,
    CreateAgent, InboxReceipt, Message, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_collect_core::WakeClass;
use bough_plugin_collector_slack::{SlackCollector, SlackCollectorConfig};
use bough_plugin_ledger::{AgentName, LedgerHandle, Order, Step, StepQuery, StepType, TrajId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_mcp::{McpCallResult, McpClient, McpError, McpHandle, McpToolInfo, ServerName};
use bough_plugin_schedule::Cadence;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;

/// The Slack MCP search payload, recorded 2026-08-29 against the live server (values
/// neutralized): a JSON object whose `results` field is rendered markdown.
pub const SEARCH: &str = include_str!("../../../../scripts/fixtures/slack/mcp-search.json");

pub fn at() -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000, 0).expect("a fixed instant")
}

pub fn traj_of(name: &str) -> TrajId {
    TrajId::new(format!("lane/{name}"))
}

/// What the stub answers with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// The recorded payload.
    Ok,
    /// A rendering the parser does not recognize.
    Drifted,
    /// An MCP `is_error` result.
    ToolError,
}

/// A stub Slack MCP server: answers the search tool and RECORDS every call, so a test can prove
/// the ordering, the page bound and the watermark actually rode the arguments.
pub struct McpStub {
    pub mode: Mode,
    pub calls: Mutex<Vec<(String, serde_json::Value)>>,
}

impl McpStub {
    pub fn new(mode: Mode) -> Arc<McpStub> {
        Arc::new(McpStub {
            mode,
            calls: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait::async_trait]
impl McpClient for McpStub {
    fn server(&self) -> &ServerName {
        static NAME: std::sync::OnceLock<ServerName> = std::sync::OnceLock::new();
        NAME.get_or_init(|| ServerName::new("slack"))
    }
    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        Ok(vec![McpToolInfo {
            server: self.server().clone(),
            tool: "slack_search_public_and_private".to_string(),
            description: String::new(),
            input_schema: serde_json::json!({ "type": "object" }),
        }])
    }
    async fn call(&self, tool: &str, args: serde_json::Value) -> Result<McpCallResult, McpError> {
        self.calls.lock().push((tool.to_string(), args));
        let (content, is_error) = match self.mode {
            Mode::Ok => (SEARCH.to_string(), false),
            Mode::Drifted => (
                r####"{"results":"### Result 1 of 1\nSomething: else\n"}"####.to_string(),
                false,
            ),
            Mode::ToolError => ("the search failed".to_string(), true),
        };
        Ok(McpCallResult {
            content,
            value: None,
            cites: Vec::new(),
            is_error,
        })
    }
}

pub struct Fx {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    pub agents: AgentsHandle,
    pub dir: tempfile::TempDir,
    pub state_db: std::path::PathBuf,
    _factory: Arc<StubFactory>,
}

impl Fx {
    pub async fn new() -> Fx {
        let ctx = Context::root(KernelCore::new());
        let dir = tempfile::tempdir().expect("a temp dir");
        let ledger = LedgerHandle(MemoryStore::new(ctx.clone()));
        let agents = AgentsHandle::new(ctx.clone(), ledger.clone());
        let factory = Arc::new(StubFactory::default());
        std::mem::forget(
            agents
                .set_factory(&ctx, factory.clone() as Arc<dyn AgentFactory>)
                .await
                .expect("the slot is free"),
        );
        Fx {
            state_db: dir.path().join("collect-slack.db"),
            ctx,
            ledger,
            agents,
            dir,
            _factory: factory,
        }
    }

    pub async fn agent(&self, name: &str) -> Agent {
        let (agent, disposer) = self
            .agents
            .create(CreateAgent::resident(
                AgentName::new(name),
                traj_of(name),
                at(),
            ))
            .await
            .expect("a fresh agent");
        std::mem::forget(disposer);
        agent
    }

    pub fn cfg(&self) -> SlackCollectorConfig {
        SlackCollectorConfig {
            cadence: Cadence::Every { every_ms: 600_000 },
            mcp_server: "slack".to_string(),
            queries: std::collections::BTreeMap::from([(
                "mentions".to_string(),
                "to:me".to_string(),
            )]),
            deliver_to: vec!["sol".to_string()],
            wake_classes: vec![WakeClass::Mention],
            state_db: self.state_db.clone(),
            batch: 20,
        }
    }

    /// The collector wired to an mcp seam holding one stub `slack` server.
    pub async fn collector(
        &self,
        cfg: SlackCollectorConfig,
        mode: Mode,
    ) -> (SlackCollector, Arc<McpStub>) {
        let mcp = McpHandle::new();
        let stub = McpStub::new(mode);
        std::mem::forget(
            mcp.server(&self.ctx, stub.clone())
                .await
                .expect("the stub registers"),
        );
        let collector =
            SlackCollector::open(Arc::new(cfg), self.ledger.clone(), self.agents.clone(), mcp)
                .expect("the row activates");
        (collector, stub)
    }

    /// The same, over an mcp seam with NO server registered.
    pub fn collector_without_server(&self, cfg: SlackCollectorConfig) -> SlackCollector {
        SlackCollector::open(
            Arc::new(cfg),
            self.ledger.clone(),
            self.agents.clone(),
            McpHandle::new(),
        )
        .expect("the row activates")
    }

    pub async fn delivered(&self, agent: &str) -> Vec<Step> {
        self.ledger
            .0
            .steps(&StepQuery {
                trajs: vec![traj_of(agent)],
                kinds: vec![StepType::new("mail/delivered")],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
            .expect("a read")
    }
}

#[derive(Default)]
pub struct StubFactory {
    pub drivers: Mutex<Vec<Arc<StubDriver>>>,
}

#[async_trait::async_trait]
impl AgentFactory for StubFactory {
    fn driver(&self) -> &'static str {
        "collector-slack-test-driver"
    }
    async fn attach(
        &self,
        cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        let d = Arc::new(StubDriver {
            _cell: cell,
            notified: Mutex::new(Vec::new()),
        });
        self.drivers.lock().push(d.clone());
        Ok(d as Arc<dyn AgentDriver>)
    }
}

pub struct StubDriver {
    _cell: AgentCell,
    pub notified: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl AgentDriver for StubDriver {
    fn driver(&self) -> &'static str {
        "collector-slack-test-driver"
    }
    async fn notify(&self, _receipt: &InboxReceipt, msg: &Message) {
        self.notified.lock().push(msg.subject.clone());
    }
    async fn cancel(&self, _cause: CancelCause, _keep_inbox: bool) {}
    async fn stop(&self) {}
    async fn wake_now(&self, _kind: WakeKind, _cause: WakeCause) -> WakeRequest {
        WakeRequest::Nothing
    }
}
