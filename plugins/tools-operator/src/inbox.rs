//! Invariant: `inbox` shows the mail this wake has NOT claimed. Once a `wake/end` consumed the
//! seqs, the same call returns nothing — reading the inbox is not what consumes it.

use std::sync::Arc;

use bough_plugin_tools::{Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome};

/// `inbox` — takes no arguments.
pub struct Inbox;

#[async_trait::async_trait]
impl Tool for Inbox {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }
    /// WP-4 owns the body.
    async fn call(&self, _call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        todo!("WP-4: unconsumed_mail for the calling agent's trajectory")
    }
}
