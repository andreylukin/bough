//! Invariant: `write` creates and echoes the new tag, so the next `patch` can chain onto it
//! without a re-view — the one legitimate way to patch a file this session never viewed.

use std::sync::Arc;

use bough_plugin_tools::{Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome};

use crate::OperatorConfig;

/// `write` — Diff render, not concurrency-safe.
pub struct Write {
    #[allow(dead_code)]
    pub cfg: Arc<OperatorConfig>,
    #[allow(dead_code)]
    pub seen: Arc<super::seen::SeenFiles>,
}

#[async_trait::async_trait]
impl Tool for Write {
    /// WP-3 owns the body.
    async fn call(&self, _call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        todo!("WP-3: contain, create, remember, echo the new tag")
    }
}
