//! Invariant (§9): a filtered-away tool and a nonexistent one produce the SAME error, message
//! included — a restriction must not be a probe for what exists elsewhere.

use bough_plugin_ledger::AgentName;
use bough_plugin_llm::ToolName;

/// What the tools registry refuses.
#[derive(Debug, thiserror::Error)]
pub enum ToolsError {
    #[error("no tool named `{name}` is available to agent `{agent}`")]
    NotFound { name: ToolName, agent: AgentName },
    #[error("tool `{name}` is already registered {scope}")]
    Duplicate { name: ToolName, scope: String },
}
