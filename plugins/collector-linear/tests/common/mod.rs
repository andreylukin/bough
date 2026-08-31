//! The shared fixture: a kernel context, a memory ledger, live agents behind a recording driver,
//! and a LOCAL GraphQL stub on `127.0.0.1:0`. Nothing in this test binary can reach the real
//! Linear API: the row's `endpoint` is the stub's URL.

#![allow(dead_code)]

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::{
    Agent, AgentCell, AgentDriver, AgentError, AgentFactory, AgentsHandle, Attach, CancelCause,
    CreateAgent, InboxReceipt, Message, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_collect_core::WakeClass;
use bough_plugin_collector_linear::{LinearCollector, LinearCollectorConfig};
use bough_plugin_ledger::{AgentName, LedgerHandle, Order, Step, StepQuery, StepType, TrajId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_mcp::{McpCallResult, McpClient, McpError, McpHandle, McpToolInfo, ServerName};
use bough_plugin_schedule::Cadence;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const ISSUES: &str = include_str!("../../../../scripts/fixtures/linear/issues.json");
pub const COMMENTS: &str = include_str!("../../../../scripts/fixtures/linear/comments.json");
/// The `linear-server` MCP payloads, recorded 2026-08-29 against the live server (values
/// neutralized). The MCP issue shape is FLAT where GraphQL's is nested.
pub const MCP_ISSUES: &str = include_str!("../../../../scripts/fixtures/linear/mcp-issues.json");
pub const MCP_COMMENTS: &str =
    include_str!("../../../../scripts/fixtures/linear/mcp-comments.json");

pub fn at() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).expect("a fixed instant")
}

pub fn traj_of(name: &str) -> TrajId {
    TrajId::new(format!("lane/{name}"))
}

/// What the stub answers with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// The recorded payloads.
    Ok,
    /// A 401 with a body that quotes nothing.
    Unauthorized,
    /// A 200 with an unparseable body.
    Garbage,
    /// A GraphQL `errors` array.
    GraphqlErrors,
}

/// A GraphQL stub on `127.0.0.1:0`. It records every `Authorization` header it saw, so a test can
/// prove the key travelled in the header and NOWHERE else.
pub struct Stub {
    pub url: String,
    pub seen: Arc<Mutex<Vec<(String, String)>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl Stub {
    pub async fn start(mode: Mode) -> Stub {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a local port");
        let addr = listener.local_addr().expect("an address");
        let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = seen.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let recorder = recorder.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    // Read the head, then exactly `Content-Length` more bytes.
                    let (head_end, len) = loop {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(pos) = find(&buf, b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                            let len: usize = head
                                .lines()
                                .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                                .and_then(|l| l.split(':').nth(1))
                                .and_then(|v| v.trim().parse().ok())
                                .unwrap_or(0);
                            break (pos + 4, len);
                        }
                    };
                    while buf.len() < head_end + len {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let body = String::from_utf8_lossy(&buf[head_end..]).to_string();
                    let auth = head
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                        .and_then(|l| l.split_once(':'))
                        .map(|(_, v)| v.trim().to_string())
                        .unwrap_or_default();
                    recorder.lock().push((auth, body.clone()));

                    let (status, payload) = match mode {
                        Mode::Unauthorized => {
                            ("401 Unauthorized", "{\"message\":\"no\"}".to_string())
                        }
                        Mode::Garbage => ("200 OK", "{not json".to_string()),
                        Mode::GraphqlErrors => (
                            "200 OK",
                            "{\"errors\":[{\"message\":\"bad query\"}]}".to_string(),
                        ),
                        Mode::Ok => {
                            if body.contains("BoughIssues") {
                                ("200 OK", ISSUES.to_string())
                            } else if body.contains("BoughComments") {
                                ("200 OK", COMMENTS.to_string())
                            } else {
                                (
                                    "400 Bad Request",
                                    "{\"message\":\"unknown query\"}".to_string(),
                                )
                            }
                        }
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = sock.write_all(response.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });
        Stub {
            url: format!("http://{addr}/graphql"),
            seen,
            _task: task,
        }
    }

    pub fn requests(&self) -> Vec<(String, String)> {
        self.seen.lock().clone()
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

pub struct Fx {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    pub agents: AgentsHandle,
    pub dir: tempfile::TempDir,
    pub state_db: std::path::PathBuf,
    _factory: Arc<StubFactory>,
}

/// The secret every test uses. It must appear in NOTHING but the `Authorization` header.
pub const KEY: &str = "lin_api_THE_SECRET_VALUE";

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
            state_db: dir.path().join("collect-linear.db"),
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

    pub fn cfg(&self, endpoint: &str) -> LinearCollectorConfig {
        LinearCollectorConfig {
            cadence: Cadence::Every { every_ms: 600_000 },
            endpoint: endpoint.to_string(),
            api_key: KEY.to_string(),
            mcp_server: String::new(),
            teams: vec!["TEAM".to_string()],
            projects: Vec::new(),
            deliver_to: vec!["sol".to_string()],
            wake_classes: vec![WakeClass::Assigned],
            state_db: self.state_db.clone(),
            batch: 50,
            timeout_ms: 10_000,
        }
    }

    /// The MCP-transport config: NO KEY anywhere, the credential belongs to the server row.
    pub fn mcp_cfg(&self) -> LinearCollectorConfig {
        LinearCollectorConfig {
            api_key: String::new(),
            mcp_server: "linear-server".to_string(),
            endpoint: "https://unused.invalid/graphql".to_string(),
            ..self.cfg("https://unused.invalid/graphql")
        }
    }

    pub fn collector(&self, cfg: LinearCollectorConfig) -> LinearCollector {
        LinearCollector::open(Arc::new(cfg), self.ledger.clone(), self.agents.clone())
            .expect("the row activates")
    }

    /// The same collector wired to an mcp seam holding one stub `linear-server`.
    pub async fn mcp_collector(
        &self,
        cfg: LinearCollectorConfig,
    ) -> (LinearCollector, Arc<McpStub>) {
        let mcp = McpHandle::new();
        let stub = Arc::new(McpStub::default());
        std::mem::forget(
            mcp.server(&self.ctx, stub.clone())
                .await
                .expect("the stub registers"),
        );
        let server = ServerName::new("linear-server");
        (self.collector(cfg).with_mcp(mcp, server), stub)
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

/// A stub `linear-server`: answers the two tools with the recorded payloads and RECORDS every
/// call, so a test can prove the viewer pin and the watermark actually rode the arguments.
#[derive(Default)]
pub struct McpStub {
    pub calls: Mutex<Vec<(String, serde_json::Value)>>,
}

#[async_trait::async_trait]
impl McpClient for McpStub {
    fn server(&self) -> &ServerName {
        static NAME: std::sync::OnceLock<ServerName> = std::sync::OnceLock::new();
        NAME.get_or_init(|| ServerName::new("linear-server"))
    }
    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        Ok(["list_issues", "list_comments"]
            .iter()
            .map(|t| McpToolInfo {
                server: self.server().clone(),
                tool: (*t).to_string(),
                description: String::new(),
                input_schema: serde_json::json!({ "type": "object" }),
            })
            .collect())
    }
    async fn call(&self, tool: &str, args: serde_json::Value) -> Result<McpCallResult, McpError> {
        self.calls.lock().push((tool.to_string(), args.clone()));
        let content = match tool {
            "list_issues" => MCP_ISSUES.to_string(),
            "list_comments" if args["issueId"] == "TEAM-123" => MCP_COMMENTS.to_string(),
            "list_comments" => r#"{"comments":[],"hasNextPage":false}"#.to_string(),
            other => return Err(McpError::Server(format!("no stub for `{other}`"))),
        };
        Ok(McpCallResult {
            content,
            value: None,
            cites: Vec::new(),
            is_error: false,
        })
    }
}

#[derive(Default)]
pub struct StubFactory {
    pub drivers: Mutex<Vec<Arc<StubDriver>>>,
}

#[async_trait::async_trait]
impl AgentFactory for StubFactory {
    fn driver(&self) -> &'static str {
        "collector-linear-test-driver"
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
        "collector-linear-test-driver"
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
