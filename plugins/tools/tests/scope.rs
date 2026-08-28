//! §9: an agent-scoped tool SHADOWS its same-named global twin — for that agent alone. Most
//! specific wins, and no other agent's view moves.

use crate::support;

use std::sync::Arc;
use std::time::Duration;

use bough_plugin_ledger::AgentName;
use bough_plugin_tools::{ToolName, ToolScope};
use parking_lot::Mutex;
use support::{agent, call, ctx, registry_with, spec, Stub};

fn stub(tag: &'static str) -> Arc<dyn bough_plugin_tools::Tool> {
    // The tag is carried by the CALL, so both twins answer with the call's tag; the shadowing is
    // proven by the description the prompt shows and by which delay is observed.
    let _ = tag;
    Arc::new(Stub {
        safe: true,
        delay: Duration::from_millis(0),
        log: Arc::new(Mutex::new(Vec::new())),
    })
}

#[tokio::test]
async fn an_agent_scoped_tool_shadows_its_global_twin_for_that_agent_only() {
    let ctx = ctx();
    let mut global = spec("bash", stub("global"));
    global.description = "the global bash".into();
    let mut scoped = spec("bash", stub("scoped"));
    scoped.description = "the lane's own bash".into();
    scoped.scope = ToolScope::Agent(agent());

    let tools = registry_with(&ctx, vec![global, scoped]).await;

    let mine = tools.schemas(&agent());
    assert_eq!(mine.len(), 1, "one name, one entry: the twin is shadowed");
    assert_eq!(mine[0].description, "the lane's own bash");

    let other = tools.schemas(&AgentName::new("other"));
    assert_eq!(other.len(), 1);
    assert_eq!(
        other[0].description, "the global bash",
        "another agent still sees the global twin"
    );
}

#[tokio::test]
async fn a_tool_scoped_to_another_agent_is_invisible_here() {
    let ctx = ctx();
    let mut theirs = spec("secret", stub("theirs"));
    theirs.scope = ToolScope::Agent(AgentName::new("other"));
    let tools = registry_with(&ctx, vec![theirs]).await;

    assert!(tools.visible(&agent()).is_empty());
    assert!(tools.resolve(&agent(), &ToolName::new("secret")).is_err());
    assert_eq!(
        tools.visible(&AgentName::new("other")),
        vec![ToolName::new("secret")]
    );
}

#[tokio::test]
async fn the_executor_runs_the_shadowing_tool() {
    let ctx = ctx();
    let global_log = Arc::new(Mutex::new(Vec::new()));
    let scoped_log = Arc::new(Mutex::new(Vec::new()));
    let global = {
        let mut s = spec(
            "bash",
            Arc::new(Stub {
                safe: true,
                delay: Duration::from_millis(0),
                log: global_log.clone(),
            }),
        );
        s.description = "global".into();
        s
    };
    let scoped = {
        let mut s = spec(
            "bash",
            Arc::new(Stub {
                safe: true,
                delay: Duration::from_millis(0),
                log: scoped_log.clone(),
            }),
        );
        s.scope = ToolScope::Agent(agent());
        s
    };
    let tools = registry_with(&ctx, vec![global, scoped]).await;

    tools.execute(&ctx, vec![call("bash", "a")]).await;
    assert!(global_log.lock().is_empty(), "the global twin never ran");
    assert_eq!(scoped_log.lock().len(), 2);
}
