//! Invariant under test: a stdio server row MOUNTS — one child entry per enabled row, an rmcp
//! client that really talks JSON-RPC to `scripts/fixtures/mcp/fixture-server.py`, and the two
//! tools it exposes reaching the seam. Disabling the child removes exactly that server.
//!
//! Nothing here touches the network: the fixture is a local python3 process.

use std::sync::Arc;

use bough_kernel::{
    Catalog, Composer, Composition, ExprEnv, Kernel, KernelOptions, LayerId, Patch,
};
use bough_plugin_mcp::{Mcp, McpToolRef, ServerName};

/// Naming the crate is what pulls its `inventory` registrations into the test binary.
const _: &str = bough_plugin_mcp_rmcp::PLUGIN_NAME;

fn fixture() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the workspace root is two levels up from the crate");
    root.join("scripts/fixtures/mcp/fixture-server.py")
        .to_string_lossy()
        .into_owned()
}

fn bundle(disabled: bool) -> String {
    format!(
        "\
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
        disabled: {disabled}
        transport: {{ kind: stdio, command: python3, args: [\"{}\"] }}
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

fn has_row(rows: &[bough_kernel::RowSnapshot], id: &str) -> bool {
    rows.iter()
        .any(|r| r.id.as_str() == id || has_row(&r.children, id))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stdio_server_row_mounts_a_child_and_its_two_tools_reach_the_seam() {
    let kernel = boot(&bundle(false)).await;
    let mcp = kernel
        .root()
        .peek_live::<Mcp>()
        .expect("the mcp seam is provided");

    assert_eq!(mcp.servers(), vec![ServerName::new("fixture")]);
    assert!(
        has_row(&kernel.rows_snapshot(), "mcp.rmcp.fixture"),
        "one child entry per enabled server row"
    );

    let mut names: Vec<String> = mcp
        .tools(None)
        .await
        .expect("the fixture lists its tools")
        .into_iter()
        .map(|t| t.tool)
        .collect();
    names.sort();
    assert_eq!(names, vec!["boom".to_string(), "echo".to_string()]);

    let echo = McpToolRef {
        server: ServerName::new("fixture"),
        tool: "echo".into(),
    };
    let args = serde_json::json!({ "text": "hi" });
    let out = mcp.call(&echo, args.clone()).await.expect("echo answers");
    assert_eq!(out.content, "echo: hi");
    assert!(!out.is_error);
    assert_eq!(
        out.cites,
        vec![bough_plugin_mcp::McpHandle::cite_of(&echo, &args)],
        "the pull's result is cited by the seam"
    );

    let boom = McpToolRef {
        server: ServerName::new("fixture"),
        tool: "boom".into(),
    };
    let bad = mcp
        .call(&boom, serde_json::json!({}))
        .await
        .expect("an MCP error result is still a result");
    assert!(bad.is_error, "is_error comes from the MCP result");

    assert!(kernel.violations().is_empty(), "{:?}", kernel.violations());
    kernel.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_server_row_mounts_no_child_and_registers_no_server() {
    let kernel = boot(&bundle(true)).await;
    let mcp = kernel.root().peek_live::<Mcp>().expect("the seam is there");
    assert!(mcp.servers().is_empty());
    assert!(!has_row(&kernel.rows_snapshot(), "mcp.rmcp.fixture"));
    kernel.shutdown().await;
}
