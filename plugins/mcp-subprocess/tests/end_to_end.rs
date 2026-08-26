//! V12, end to end: a resident subprocess mounted by `mcp-subprocess` shows up on `ctx.tools`
//! through `tool-mcp`, and after the OS process is killed the SAME tool name is still registered
//! and answers a real call again once the supervisor has respawned it.
//!
//! The other suite in this crate asserts on `ResidentProcess` directly. This one never touches the
//! client: it goes through the kernel, `ctx.mcp` and the tools seam, and the only thing it does
//! out of band is `kill -9` on the pid the fixture recorded.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bough_kernel::{
    Catalog, Composer, Composition, ExprEnv, Kernel, KernelOptions, LayerId, Patch, RowSnapshot,
};
use bough_plugin_ledger::{AgentName, WakeId};
use bough_plugin_tools::{ToolCall, ToolCallId, ToolName, Tools};

/// Naming the crates is what pulls their `inventory` registrations into this test binary.
const _: (&str, &str, &str, &str, &str) = (
    bough_plugin_mcp_subprocess::PLUGIN_NAME,
    bough_plugin_tool_mcp::PLUGIN_NAME,
    bough_plugin_ledger_memory::PLUGIN_NAME,
    bough_plugin_mcp::PLUGIN_NAME,
    bough_plugin_schedule_manual::PLUGIN_NAME,
);

fn server() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/fixtures/mcp-process/echo-server.py")
        .canonicalize()
        .expect("the fixture server exists")
        .display()
        .to_string()
}

fn bundle(record: &std::path::Path) -> String {
    format!(
        "\
- id: ledger
  plugin: ledger-memory
  config: {{}}
- id: agents
  plugin: agents
  inject: [ledger]
  config: {{}}
- id: actions
  plugin: actions
  inject: [ledger]
  config: {{}}
- id: schedule
  plugin: schedule-manual
  config: {{}}
- id: workers
  plugin: workers
  config: {{ max_in_flight: 4, max_depth: 2, per_wake_spawn_cap: 2 }}
- id: tools
  plugin: tools
  config: {{ default_deadline_ms: 10000, max_parallel: 2 }}
- id: mcp
  plugin: mcp
  config: {{}}
- id: tool.mcp
  plugin: tool-mcp
  inject: [mcp, tools]
  config: {{ prefix: \"mcp__\", max_result_bytes: 20000 }}
- id: mcp.subprocess
  plugin: mcp-subprocess
  inject: [mcp, ledger, agents, actions, workers, schedule]
  config:
    limits: {{ max_actions: 16, max_spawns: 2, max_text_bytes: 8192 }}
    processes:
      - name: echo
        command: python3
        args: [\"{}\"]
        env: {{ MCP_FIXTURE_RECORD: \"{}\" }}
        max_restarts: 3
        min_uptime_ms: 1000
        restart_delay_ms: 20
        call_timeout_ms: 5000
        boot_timeout_ms: 10000
",
        server(),
        record.display()
    )
}

async fn boot(yaml: &str) -> Arc<Kernel> {
    let catalog = Catalog::from_inventory().expect("the linked catalog has no duplicate names");
    let patch: Patch = serde_yaml::from_str(yaml).expect("the test bundle parses");
    let mut composer = Composer::new(&catalog, ExprEnv::new("test"));
    composer.layer(LayerId::new("test"), patch);
    let composition: Composition = composer.compose().expect("the test bundle composes");
    let kernel = Kernel::new(
        catalog,
        KernelOptions {
            profile: "test".into(),
            invariants: false,
        },
    );
    kernel.load(composition).await.expect("the tree mounts");
    kernel.quiesce().await;
    kernel
}

fn flat(rows: &[RowSnapshot], out: &mut Vec<RowSnapshot>) {
    for r in rows {
        out.push(r.clone());
        flat(&r.children, out);
    }
}

fn visible(kernel: &Kernel) -> Vec<String> {
    kernel
        .root()
        .peek_live::<Tools>()
        .expect("the tools seam")
        .visible(&AgentName::new("sol"))
        .into_iter()
        .map(|n| n.to_string())
        .collect()
}

async fn call(kernel: &Kernel, id: &str, text: &str) -> bough_plugin_tools::ToolResult {
    let tools = kernel.root().peek_live::<Tools>().expect("the tools seam");
    let root = kernel.root();
    tools
        .execute(
            &root,
            vec![ToolCall {
                id: ToolCallId::new(id),
                name: ToolName::new("mcp__echo__echo"),
                args: serde_json::json!({ "text": text }),
                agent: AgentName::new("sol"),
                wake: WakeId::new("w1"),
                step_index: 1,
            }],
        )
        .await
        .pop()
        .expect("one call, one result")
}

/// Poll until `f` holds or the deadline passes.
async fn until(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

fn pids(record: &std::path::Path) -> Vec<u32> {
    std::fs::read_to_string(record)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1)?.parse().ok())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resident_subprocess_mounts_one_child_and_its_tool_survives_a_crash_on_the_tools_seam() {
    let dir = tempfile::tempdir().expect("tempdir");
    let record = dir.path().join("starts");
    let kernel = boot(&bundle(&record)).await;

    // ONE child entry, named for the server.
    let mut rows = Vec::new();
    flat(&kernel.snapshot().rows, &mut rows);
    let children: Vec<_> = rows
        .iter()
        .filter(|r| r.id.as_str().starts_with("mcp.subprocess."))
        .map(|r| r.id.as_str().to_string())
        .collect();
    assert_eq!(
        children,
        vec!["mcp.subprocess.echo".to_string()],
        "{rows:?}"
    );

    // Its tool is on ctx.tools, and it answers through the tools seam.
    assert!(
        visible(&kernel).contains(&"mcp__echo__echo".to_string()),
        "{:?}",
        visible(&kernel)
    );
    let before = call(&kernel, "c1", "hello").await;
    assert!(before.ok, "{:?}", before.failure);
    assert_eq!(before.content, "hello");

    // Kill the OS process out of band.
    let first = pids(&record);
    assert_eq!(
        first.len(),
        1,
        "the fixture recorded its one start: {first:?}"
    );
    std::process::Command::new("kill")
        .arg("-9")
        .arg(first[0].to_string())
        .status()
        .expect("kill");

    // A second OS process comes back on its own, and the tool NEVER left the seam meanwhile.
    assert!(
        until(8000, || pids(&record).len() >= 2).await,
        "the supervisor respawned it: {:?}",
        pids(&record)
    );
    let second = pids(&record);
    assert_ne!(second[1], first[0], "a genuinely new process");
    assert!(
        visible(&kernel).contains(&"mcp__echo__echo".to_string()),
        "the registration outlived the restart"
    );

    // And it answers a real call again — through the seam, over the NEW process's stdio.
    let mut last = None;
    for i in 0..40u32 {
        let r = call(&kernel, &format!("c2-{i}"), "again").await;
        if r.ok {
            last = Some(r);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let after = last.expect("the tool answers again after the restart");
    assert_eq!(after.content, "again");

    kernel.shutdown().await;
}
