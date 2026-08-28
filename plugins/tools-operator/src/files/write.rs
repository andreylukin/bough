//! Invariant: `write` creates and echoes the new tag, so the next `patch` can chain onto it
//! without a re-view — the one legitimate way to patch a file this session never viewed.

use std::sync::Arc;

use bough_plugin_tools::{
    FailureClass, Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome, WorkspaceRoot,
};

use crate::OperatorConfig;

use super::apply::plural;
use super::grammar::{tag_of, to_lines};
use super::seen::SeenFiles;
use super::view::{arg_str, err};

/// `write` — Diff render, not concurrency-safe.
pub struct Write {
    #[allow(dead_code)]
    pub cfg: Arc<OperatorConfig>,
    pub root: WorkspaceRoot,
    pub seen: Arc<SeenFiles>,
}

#[async_trait::async_trait]
impl Tool for Write {
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let path = arg_str(&call, "path")?;
        let content = arg_str(&call, "content")?;
        if path.trim().is_empty() {
            return Err(err(
                FailureClass::Error,
                "write needs a path — pass one relative to the workspace, or an absolute one \
                 inside it.",
            ));
        }
        let abs = super::contain(&self.root, &path).map_err(|m| err(FailureClass::Denied, m))?;
        // Parent directories are created: the alternative is a program that must shell out to
        // `mkdir -p` before every new file.
        if let Some(dir) = abs.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                err(
                    FailureClass::Error,
                    format!("cannot create {}: {e}", dir.display()),
                )
            })?;
        }
        std::fs::write(&abs, content.as_bytes())
            .map_err(|e| err(FailureClass::Error, format!("cannot write {path}: {e}")))?;

        // Recording is what lets a freshly written file be patched with `[path#]` in the same
        // round.
        let tag = tag_of(&content);
        self.seen.remember(
            call.agent.clone(),
            abs.clone(),
            tag.clone(),
            content.clone(),
        );

        Ok(ToolOutcome {
            content: format!(
                "[{path}#{tag}] wrote {} ({})",
                plural(to_lines(&content).len(), "line"),
                plural(content.len(), "byte")
            ),
            value: None,
            cites: vec![super::file_cite(&abs)],
            concludes_wake: false,
        })
    }
}
