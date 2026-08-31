//! Shared fixtures for the tools-seam tests: a stub tool that records when it starts and ends,
//! and the small builders every case needs.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{AgentName, WakeId};
use bough_plugin_tools::{
    RenderIntent, Tool, ToolCall, ToolCallId, ToolCx, ToolFailure, ToolName, ToolOutcome,
    ToolScope, ToolSpec, ToolsHandle,
};
use parking_lot::Mutex;

pub fn ctx() -> Context {
    Context::root(KernelCore::new())
}

pub fn agent() -> AgentName {
    AgentName::new("lane")
}

/// A tool that logs `start <tag>` / `end <tag>` around a sleep, so overlap and barriers are
/// observable facts rather than timing guesses.
pub struct Stub {
    pub safe: bool,
    pub delay: Duration,
    pub log: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl Tool for Stub {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        self.safe
    }
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let tag = call
            .args
            .get("tag")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        self.log.lock().push(format!("start {tag}"));
        tokio::time::sleep(self.delay).await;
        self.log.lock().push(format!("end {tag}"));
        Ok(ToolOutcome {
            content: tag,
            value: None,
            cites: vec![],
            concludes_wake: false,
        })
    }
}

pub fn spec(name: &str, tool: Arc<dyn Tool>) -> ToolSpec {
    ToolSpec {
        name: ToolName::new(name),
        description: format!("the {name} tool"),
        input_schema: schemars::Schema::try_from(serde_json::json!({ "type": "object" })).unwrap(),
        render: RenderIntent::Generic,
        scope: ToolScope::Global,
        tool,
    }
}

pub fn call(name: &str, tag: &str) -> ToolCall {
    ToolCall {
        id: ToolCallId::new(format!("call-{tag}")),
        name: ToolName::new(name),
        args: serde_json::json!({ "tag": tag }),
        agent: agent(),
        wake: WakeId::new("w1"),
        step_index: 1,
    }
}

/// A registry with one always-succeeding tool named `echo`, and generous limits.
pub async fn registry_with(ctx: &Context, specs: Vec<ToolSpec>) -> ToolsHandle {
    let tools = ToolsHandle::with_limits(8, 5_000);
    for s in specs {
        tools.register(ctx, s).await.unwrap();
    }
    tools
}
