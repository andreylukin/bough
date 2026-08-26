//! Invariant under test: the tools a real stdio MCP server exposes appear on `ctx.tools` under
//! `mcp__<server>__<tool>`, a call's `ToolResult` carries the SEAM's cite, and disabling the
//! server's row removes exactly those tools and leaves every other registration alone.
//!
//! Hermetic: the server is `scripts/fixtures/mcp/fixture-server.py`, a local python3 process.

use std::sync::Arc;

use bough_kernel::{
    Catalog, Composer, Composition, Context, ExprEnv, Kernel, KernelOptions, LayerId, Patch,
};
use bough_plugin_commands::{CommandCx, CommandError, CommandName, Commands, Invocation};
use bough_plugin_ledger::{AgentName, WakeId};
use bough_plugin_mcp::Mcp;
use bough_plugin_tools::{
    AgentId, RenderIntent, Tool, ToolCall, ToolCallId, ToolCx, ToolFailure, ToolName, ToolOutcome,
    ToolScope, ToolSpec, Tools,
};

/// Naming the crates is what pulls their `inventory` registrations into the test binary.
const _: (&str, &str, &str) = (
    bough_plugin_tool_mcp::PLUGIN_NAME,
    bough_plugin_mcp_rmcp::PLUGIN_NAME,
    bough_plugin_ledger_memory::PLUGIN_NAME,
);

fn fixture() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the workspace root is two levels up");
    root.join("scripts/fixtures/mcp/fixture-server.py")
        .to_string_lossy()
        .into_owned()
}

fn bundle(server_disabled: bool) -> String {
    format!(
        "\
- id: ledger
  plugin: ledger-memory
  config: {{}}
- id: tools
  plugin: tools
  config: {{ default_deadline_ms: 10000, max_parallel: 2 }}
- id: commands
  plugin: commands
  config: {{ prefix: \"/\", suggestions: true }}
- id: mcp
  plugin: mcp
  config: {{}}
- id: mcp.rmcp
  plugin: mcp-rmcp
  config:
    connect_timeout_ms: 10000
    call_timeout_ms: 10000
    servers:
      - name: fixture
        disabled: {server_disabled}
        transport: {{ kind: stdio, command: python3, args: [\"{}\"] }}
- id: tool.mcp
  plugin: tool-mcp
  config: {{ prefix: \"mcp__\", max_result_bytes: 20000 }}
",
        fixture()
    )
}

fn compose(catalog: &Catalog, yaml: &str) -> Composition {
    let patch: Patch = serde_yaml::from_str(yaml).expect("the test bundle parses");
    let mut composer = Composer::new(catalog, ExprEnv::new("test"));
    composer.layer(LayerId::new("test"), patch);
    composer.compose().expect("the test bundle composes")
}

/// Several kernels in ONE test binary mint colliding `FiberUid`s, so the boots that assert on
/// invariant violations take this lock and clear the stream first. Without it a second kernel's
/// `tool.mcp` looks like the first one's, and its registrations look like duplicates.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn boot(yaml: &str) -> Arc<Kernel> {
    let catalog = Catalog::from_inventory().expect("the linked catalog has no duplicate names");
    let composition = compose(&catalog, yaml);
    let kernel = Kernel::new(
        catalog,
        KernelOptions {
            profile: "test".into(),
            invariants: true,
        },
    );
    kernel.load(composition).await.expect("the tree mounts");
    kernel.quiesce().await;
    kernel
}

async fn update(kernel: &Kernel, yaml: &str) {
    let catalog = Catalog::from_inventory().expect("catalog");
    let composition = compose(&catalog, yaml);
    kernel.update(composition).await.expect("the tree updates");
    kernel.quiesce().await;
}

/// A tool registered by somebody else, so "leaves the rest of the registry" has a subject.
struct Other;

#[async_trait::async_trait]
impl Tool for Other {
    async fn call(&self, _c: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        Ok(ToolOutcome::default())
    }
}

fn other_spec() -> ToolSpec {
    ToolSpec {
        name: ToolName::new("someone_elses_tool"),
        description: "not an MCP tool".into(),
        input_schema: schemars::Schema::try_from(serde_json::json!({ "type": "object" })).unwrap(),
        render: RenderIntent::Generic,
        scope: ToolScope::Global,
        tool: Arc::new(Other),
    }
}

fn has_row(rows: &[bough_kernel::RowSnapshot], id: &str) -> bool {
    rows.iter()
        .any(|r| r.id.as_str() == id || has_row(&r.children, id))
}

fn agent() -> AgentName {
    AgentName::new("sol")
}

fn visible(kernel: &Kernel) -> Vec<String> {
    kernel
        .root()
        .peek_live::<Tools>()
        .expect("the tools seam")
        .visible(&agent())
        .into_iter()
        .map(|n| n.to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fixture_servers_two_tools_appear_on_the_tools_seam_and_a_call_carries_the_mcp_cite() {
    let _serial = SERIAL.lock().await;
    bough_plugin_tool_mcp::invariant::reset();
    let kernel = boot(&bundle(false)).await;
    let names = visible(&kernel);
    assert!(
        names.contains(&"mcp__fixture__echo".to_string())
            && names.contains(&"mcp__fixture__boom".to_string()),
        "{names:?}"
    );

    let tools = kernel.root().peek_live::<Tools>().unwrap();
    let root = kernel.root();
    let args = serde_json::json!({ "text": "hi" });
    let result = tools
        .execute(
            &root,
            vec![ToolCall {
                id: ToolCallId::new("c1"),
                name: ToolName::new("mcp__fixture__echo"),
                args: args.clone(),
                agent: agent(),
                wake: WakeId::new("w1"),
                step_index: 1,
            }],
        )
        .await
        .pop()
        .expect("one call, one result");
    assert!(result.ok, "{:?}", result.failure);
    assert_eq!(result.content, "echo: hi");
    let expected = bough_plugin_mcp::McpHandle::cite_of(
        &bough_plugin_mcp::McpToolRef {
            server: bough_plugin_mcp::ServerName::new("fixture"),
            tool: "echo".into(),
        },
        &args,
    );
    assert_eq!(
        result.cites,
        vec![expected],
        "the pull is cited by the seam"
    );

    // Only this crate's own invariant is asserted on: the invariant streams of the crates this
    // test boots alongside it are process-global, and three kernels in one test binary make them
    // report each other's rows.
    let mine: Vec<_> = kernel
        .violations()
        .into_iter()
        .filter(|v| v.plugin == bough_plugin_tool_mcp::PLUGIN_NAME)
        .collect();
    assert!(mine.is_empty(), "{mine:?}");
    kernel.shutdown().await;
    let _: Option<AgentId> = None;
}

#[tokio::test(flavor = "multi_thread")]
async fn disabling_the_server_row_removes_exactly_its_tools() {
    let _serial = SERIAL.lock().await;
    let kernel = boot(&bundle(false)).await;

    // Somebody else's registration, made against the root context so it survives the update.
    let tools = kernel.root().peek_live::<Tools>().unwrap();
    let root: Context = kernel.root();
    tools.register(&root, other_spec()).await.unwrap();
    assert!(visible(&kernel).contains(&"mcp__fixture__echo".to_string()));

    update(&kernel, &bundle(true)).await;

    let after = visible(&kernel);
    assert!(
        !after.iter().any(|n| n.starts_with("mcp__")),
        "every MCP tool is gone: {after:?}"
    );
    assert!(
        after.contains(&"someone_elses_tool".to_string()),
        "and nothing else was touched: {after:?}"
    );
    assert!(kernel
        .root()
        .peek_live::<Mcp>()
        .unwrap()
        .servers()
        .is_empty());
    kernel.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_mcp_command_calls_lists_and_refuses_malformed_json() {
    let _serial = SERIAL.lock().await;
    let kernel = boot(&bundle(false)).await;
    let commands = kernel.root().peek_live::<Commands>().expect("commands");

    let run = |line: String| {
        let commands = commands.clone();
        let root = kernel.root();
        let args: Vec<String> = line.split(' ').map(String::from).collect();
        async move {
            commands
                .dispatch(
                    Invocation {
                        name: CommandName::new("mcp"),
                        raw: format!("/mcp {line}"),
                        args,
                    },
                    CommandCx {
                        ctx: root,
                        agent: None,
                        at: chrono::Utc::now(),
                    },
                )
                .await
        }
    };

    let listed = run("list".to_string()).await.expect("`list` renders");
    assert!(listed.text.contains("fixture__echo"), "{}", listed.text);

    let called = run("call fixture echo {\"text\":\"hi\"}".to_string())
        .await
        .expect("`call` renders");
    assert!(called.text.contains("echo: hi"), "{}", called.text);
    assert_eq!(called.cites.len(), 1, "the output cites the pull");
    assert!(called.cites[0]
        .r#ref
        .to_string()
        .starts_with("mcp:fixture:echo:"));

    match run("call fixture echo {oops".to_string())
        .await
        .unwrap_err()
    {
        CommandError::BadArgs { usage, detail } => {
            assert_eq!(usage, bough_plugin_tool_mcp::command::USAGE);
            assert!(detail.contains("not JSON"), "{detail}");
        }
        other => panic!("expected BadArgs, got {other:?}"),
    }

    // The TOOL'S schema, not the server, is what refuses a missing required argument.
    match run("call fixture echo {}".to_string()).await.unwrap_err() {
        CommandError::BadArgs { detail, .. } => assert!(detail.contains("text"), "{detail}"),
        other => panic!("expected BadArgs, got {other:?}"),
    }

    kernel.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_mcp_call_row_with_an_empty_server_mounts_and_does_nothing() {
    let yaml = "\
- id: mcp
  plugin: mcp
  config: {}
- id: mcp.call
  plugin: mcp-call
  config: { server: \"\", tool: \"\", args: \"\", print: text, exit_when_done: false }
";
    let kernel = boot(yaml).await;
    assert!(kernel
        .root()
        .peek_live::<Mcp>()
        .unwrap()
        .servers()
        .is_empty());
    assert!(
        has_row(&kernel.rows_snapshot(), "mcp.call"),
        "the row mounted"
    );
    kernel.shutdown().await;
}
