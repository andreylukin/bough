//! Invariant: a ledger drill is EVIDENCE. Every result cites the steps it came from, so reading
//! the past is as citable as observing the present.

use std::sync::Arc;

use bough_plugin_tools::{Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome};

use crate::OperatorConfig;

/// One tool — `{op: "search"|"steps"|"tail", ...}` — sugared as the `ledger` namespace:
/// `ledger.search(q)` / `ledger.steps(range)` / `ledger.tail(n)`. The point is drilling from a
/// tier's `notable_refs` down to the raw steps behind them.
pub struct LedgerRead {
    #[allow(dead_code)]
    pub cfg: Arc<OperatorConfig>,
}

#[async_trait::async_trait]
impl Tool for LedgerRead {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }
    /// WP-4 owns the body.
    async fn call(&self, _call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        todo!("WP-4: search | steps | tail over LedgerHandle, paged by ledger_page, cited")
    }
}
