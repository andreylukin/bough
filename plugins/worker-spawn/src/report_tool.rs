//! Invariant (§10): a worker's report reaches its spawner through ONE named door — the `report`
//! tool, whose input schema IS the seal. A blob that does not validate never becomes a report:
//! the tool refuses it and the worker is told which pointer failed, so the seal is a contract the
//! model can actually satisfy rather than a post-hoc rejection.

use std::sync::Arc;

use bough_plugin_tools::{
    FailureClass, RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolName, ToolOutcome,
    ToolScope, ToolSpec,
};
use bough_plugin_workers::SealSpec;
use parking_lot::Mutex;

/// Where a validated report lands. One slot: §10 says a worker reports once.
#[derive(Clone, Default)]
pub struct ReportSlot(Arc<Mutex<Option<serde_json::Value>>>);

impl ReportSlot {
    pub fn new() -> ReportSlot {
        ReportSlot::default()
    }
    pub fn take(&self) -> Option<serde_json::Value> {
        self.0.lock().take()
    }
    pub fn is_filled(&self) -> bool {
        self.0.lock().is_some()
    }
}

/// The scoped `report` tool.
pub struct ReportTool {
    seal: SealSpec,
    slot: ReportSlot,
}

impl ReportTool {
    pub fn new(seal: SealSpec, slot: ReportSlot) -> ReportTool {
        ReportTool { seal, slot }
    }

    /// The registration this tool needs: scoped to ONE agent, so no other agent can report for
    /// this worker, and carrying the seal as its input schema.
    pub fn spec(
        agent: bough_plugin_ledger::AgentName,
        seal: SealSpec,
        slot: ReportSlot,
    ) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("report"),
            description: "Report the result of your task to the agent that started you. Call this \
                          exactly once, when the task is done. Every claim must carry a citation \
                          to something you actually observed; a claim you cannot cite is recorded \
                          as a thought, not as a finding."
                .to_string(),
            input_schema: seal.schema.as_ref().clone(),
            render: RenderIntent::Generic,
            scope: ToolScope::Agent(agent),
            tool: Arc::new(ReportTool::new(seal, slot)),
        }
    }
}

#[async_trait::async_trait]
impl Tool for ReportTool {
    /// Reporting ends the wake, and a second report while the first is in flight is meaningless.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }

    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        if let Err(detail) = self.seal.validate(&call.args) {
            return Err(ToolFailure {
                kind: FailureClass::Error,
                message: format!(
                    "your report does not match the `{}` seal: {detail}",
                    self.seal.name
                ),
            });
        }
        if self.slot.is_filled() {
            return Err(ToolFailure {
                kind: FailureClass::Blocked,
                message: "you have already reported; a worker reports once".to_string(),
            });
        }
        *self.slot.0.lock() = Some(call.args.clone());
        Ok(ToolOutcome {
            content: "reported".to_string(),
            value: None,
            cites: Vec::new(),
            // §5: the wake ends at this step. There is nothing left for the worker to do.
            concludes_wake: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx() -> ToolCx {
        ToolCx {
            ctx: bough_kernel::Context::root(bough_kernel::KernelCore::new()),
            cancel: tokio_util::sync::CancellationToken::new(),
            deadline: None,
            initiator: None,
        }
    }

    fn call(args: serde_json::Value) -> Arc<ToolCall> {
        Arc::new(ToolCall {
            id: bough_plugin_tools::ToolCallId::new("c1"),
            name: ToolName::new("report"),
            args,
            agent: bough_plugin_ledger::AgentName::new("sol/worker-1"),
            wake: bough_plugin_ledger::WakeId::new("w"),
            step_index: 0,
        })
    }

    #[tokio::test]
    async fn a_sealed_report_fills_the_slot_and_concludes_the_wake() {
        let slot = ReportSlot::new();
        let tool = ReportTool::new(SealSpec::report(), slot.clone());
        let out = tool
            .call(
                call(serde_json::json!({ "summary": "done", "claims": [] })),
                cx(),
            )
            .await
            .expect("a sealed report is accepted");
        assert!(out.concludes_wake);
        assert!(slot.take().is_some());
    }

    #[tokio::test]
    async fn an_unsealed_report_is_refused_and_the_slot_stays_empty() {
        let slot = ReportSlot::new();
        let tool = ReportTool::new(SealSpec::report(), slot.clone());
        let err = tool
            .call(call(serde_json::json!({ "claims": [] })), cx())
            .await
            .expect_err("no summary is not a report");
        assert!(err.message.contains("summary"), "{}", err.message);
        assert!(!slot.is_filled());
    }

    #[tokio::test]
    async fn a_second_report_is_blocked() {
        let slot = ReportSlot::new();
        let tool = ReportTool::new(SealSpec::report(), slot.clone());
        let ok = serde_json::json!({ "summary": "done", "claims": [] });
        tool.call(call(ok.clone()), cx()).await.expect("first");
        let err = tool.call(call(ok), cx()).await.expect_err("second");
        assert_eq!(err.kind, FailureClass::Blocked);
    }
}
