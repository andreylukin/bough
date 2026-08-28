//! WP-2: what the model is SHOWN and what it can REACH.
//!
//! Concealment is visibility: after the row mounts, the request's tool list is `run` alone, and
//! everything the agent could call before is still callable from inside a program. A restriction
//! another row owns (a lane's `deny`) is never lifted — the tool it removed is absent from the
//! injected globals AND `NotFound` at the mirror.

mod support;

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_plugin_ledger::WakeId;
use bough_plugin_tools::{FailureClass, Restrict, ToolCall, ToolCallId, ToolName};
use bough_plugin_tools_codemode::bind::bindings;
use bough_plugin_tools_codemode::conceal::visible_specs;
use support::{agent, config, harness, spec, Echo};

fn echo() -> Arc<dyn bough_plugin_tools::Tool> {
    Arc::new(Echo { concludes: false })
}

fn deny(names: &[&str]) -> Restrict {
    Restrict {
        allow: None,
        deny: names
            .iter()
            .map(|n| ToolName::new(*n))
            .collect::<BTreeSet<_>>(),
    }
}

#[tokio::test]
async fn the_request_shows_run_alone_and_the_program_still_reaches_everything() {
    let h = harness(vec![spec("echo", echo()), spec("other", echo())], config()).await;
    let shown: Vec<String> = h
        .tools
        .schemas(&agent())
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(shown, vec!["run".to_string()], "one API tool, no schemas");

    let out = h.program("call echo []\ncall other []").await.unwrap();
    assert!(out.content.contains("echo said"), "{:?}", out.content);
    assert!(out.content.contains("other said"), "{:?}", out.content);
}

#[tokio::test]
async fn a_lane_restricted_tool_is_absent_from_the_globals_and_not_found_at_the_mirror() {
    let h = harness(vec![spec("echo", echo()), spec("secret", echo())], config()).await;
    // A restriction another row owns. `install` already ran, so this composes with it exactly as
    // a lane's `deny` composes with the row's `allow: {run}`.
    h.tools
        .restrict(&h.ctx, &agent(), deny(&["secret"]))
        .await
        .unwrap();

    // 1. absent from the injected globals.
    let mirror = h
        .conceal
        .snapshot(&h.ctx, &h.tools, &agent(), 1_000)
        .await
        .unwrap();
    let names: Vec<String> = bindings(&mirror.specs, &Default::default(), &Default::default())
        .unwrap()
        .into_iter()
        .map(|b| b.js)
        .collect();
    assert_eq!(
        names,
        vec!["echo".to_string()],
        "the denied tool is not injected"
    );

    // 2. NotFound at the mirror, so the absence is not merely cosmetic.
    let result = mirror
        .tools
        .execute(
            &h.ctx,
            vec![ToolCall {
                id: ToolCallId::new("call_1.0"),
                name: ToolName::new("secret"),
                args: serde_json::json!({}),
                agent: agent(),
                wake: WakeId::new("w1"),
                step_index: 1,
            }],
        )
        .await;
    assert_eq!(result.len(), 1);
    assert!(!result[0].ok);
    assert_eq!(
        result[0].failure.as_ref().map(|f| f.kind),
        Some(FailureClass::NotFound)
    );
    mirror.dispose().await;

    // 3. and the program itself never sees the name.
    let out = h.program("call secret []").await.unwrap();
    assert!(
        out.content.contains("undefined secret"),
        "{:?}",
        out.content
    );
}

#[tokio::test]
async fn an_alias_and_a_namespace_reach_the_registered_tool() {
    let mut cfg = config();
    cfg.aliases
        .insert("claim".to_string(), "propose_claim".to_string());
    cfg.namespaces
        .insert("mcp".to_string(), "mcp__".to_string());
    let h = harness(
        vec![
            spec("propose_claim", echo()),
            spec("mcp__linear__issues", echo()),
        ],
        cfg,
    )
    .await;

    let out = h
        .program("call claim [{\"kind\":\"lane\"}]\ncall mcp.linear.issues []")
        .await
        .unwrap();
    assert!(
        out.content.contains("propose_claim said"),
        "{:?}",
        out.content
    );
    assert!(
        out.content.contains("mcp__linear__issues said"),
        "{:?}",
        out.content
    );
    // The default name of an aliased tool is not injected.
    let out = h.program("call propose_claim []").await.unwrap();
    assert!(
        out.content.contains("undefined propose_claim"),
        "{:?}",
        out.content
    );
}

#[tokio::test]
async fn the_snapshot_is_retaken_per_program_so_a_late_tool_is_reachable() {
    let h = harness(vec![spec("echo", echo())], config()).await;
    let out = h.program("call late []").await.unwrap();
    assert!(out.content.contains("undefined late"), "{:?}", out.content);

    h.tools
        .register(&h.ctx, spec("late", echo()))
        .await
        .unwrap();
    let out = h.program("call late []").await.unwrap();
    assert!(out.content.contains("late said"), "{:?}", out.content);
}

#[tokio::test]
async fn run_is_never_in_the_snapshot_so_a_program_cannot_nest_a_program() {
    let h = harness(vec![spec("echo", echo())], config()).await;
    let specs = visible_specs(&h.tools, &agent());
    assert!(
        !specs.iter().any(|s| s.name.as_str() == "run"),
        "`run` must not be injectable into its own sandbox"
    );
}
