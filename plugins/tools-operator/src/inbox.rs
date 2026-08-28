//! Invariant: `inbox` shows the mail this wake has NOT claimed. Once a `wake/end` consumed the
//! seqs, the same call returns nothing — reading the inbox is not what consumes it.

use std::sync::Arc;

use bough_plugin_ledger::{AgentName, LedgerHandle, Step, TrajId};
use bough_plugin_tools::{FailureClass, Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome};

/// `inbox` — takes no arguments.
pub struct Inbox {
    pub ledger: LedgerHandle,
    /// Present when the `agents` row is mounted: a live agent knows its own head trajectory
    /// without a membership query. Absent, the trajectory is derived from the ledger.
    pub agents: Option<bough_plugin_agents::AgentsHandle>,
}

fn err(kind: FailureClass, message: impl Into<String>) -> ToolFailure {
    ToolFailure {
        kind,
        message: message.into(),
    }
}

/// The agent's own trajectory: the live agent's head if there is one, else `connected().own`.
pub async fn own_traj(
    ledger: &LedgerHandle,
    agents: Option<&bough_plugin_agents::AgentsHandle>,
    agent: &AgentName,
) -> Result<Option<TrajId>, ToolFailure> {
    if let Some(a) = agents.and_then(|h| h.by_name(agent)) {
        return Ok(Some(a.traj().clone()));
    }
    let connected = ledger
        .0
        .connected(agent)
        .await
        .map_err(|e| err(FailureClass::Error, e.to_string()))?;
    Ok((!connected.is_rowless()).then_some(connected.own))
}

/// Render the unconsumed mail. Pure, so the fold is testable without a tool call.
pub fn render(mail: &[Step]) -> ToolOutcome {
    if mail.is_empty() {
        return ToolOutcome {
            content: "inbox: nothing unconsumed".to_string(),
            value: Some(serde_json::json!({ "count": 0, "mail": [] })),
            ..Default::default()
        };
    }
    let mut lines = vec![format!("inbox: {} unconsumed", mail.len())];
    let mut cites = Vec::new();
    let mut items = Vec::new();
    for step in mail {
        let from = step
            .body
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let subject = step
            .body
            .get("subject")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let summary = step
            .body
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        lines.push(format!(
            "#{} from {from}: {subject} — {summary}",
            step.seq.0
        ));
        cites.push(crate::ledger_read::step_cite(step));
        items.push(serde_json::json!({
            "seq": step.seq.0,
            "from": from,
            "subject": subject,
            "summary": summary,
        }));
    }
    ToolOutcome {
        content: lines.join("\n"),
        value: Some(serde_json::json!({ "count": items.len(), "mail": items })),
        cites,
        concludes_wake: false,
    }
}

#[async_trait::async_trait]
impl Tool for Inbox {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let Some(traj) = own_traj(&self.ledger, self.agents.as_ref(), &call.agent).await? else {
            return Ok(render(&[]));
        };
        let mail = self
            .ledger
            .0
            .unconsumed_mail(&traj)
            .await
            .map_err(|e| err(FailureClass::Error, e.to_string()))?;
        Ok(render(&mail))
    }
}
