//! Invariant: what `view` returns is exactly what the patch grammar's line numbers refer to.

use std::sync::Arc;

use bough_plugin_tools::{Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome};

use crate::OperatorConfig;

/// `view` — Generic render, concurrency-safe. Returns `[path#TAG]` plus `N:text` rows and
/// remembers the text in [`super::seen::SeenFiles`].
pub struct View {
    #[allow(dead_code)]
    pub cfg: Arc<OperatorConfig>,
    #[allow(dead_code)]
    pub seen: Arc<super::seen::SeenFiles>,
}

#[async_trait::async_trait]
impl Tool for View {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }
    /// WP-3 owns the body.
    async fn call(&self, _call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        todo!("WP-3: contain the path, read, normalize, render_numbered, remember")
    }
}

/// `patch` — Diff render, not concurrency-safe.
pub struct Patch {
    #[allow(dead_code)]
    pub cfg: Arc<OperatorConfig>,
    #[allow(dead_code)]
    pub seen: Arc<super::seen::SeenFiles>,
}

#[async_trait::async_trait]
impl Tool for Patch {
    /// WP-3 owns the body.
    async fn call(&self, _call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        todo!("WP-3: apply_patch and echo each file's new tag")
    }
}
