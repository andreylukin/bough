//! Invariant: what `view` returns is exactly what the patch grammar's line numbers refer to, and
//! rendering it RECORDS it — a tag naming a version nothing can produce again would be a lie.

use std::sync::Arc;

use bough_plugin_tools::{
    FailureClass, Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome, WorkspaceRoot,
};

use crate::OperatorConfig;

use super::apply::{apply_patch, echo};
use super::grammar::{group_by_file, parse_patch, render_numbered, tag_of, PatchError};
use super::seen::SeenFiles;

pub(crate) fn err(kind: FailureClass, message: impl Into<String>) -> ToolFailure {
    ToolFailure {
        kind,
        message: message.into(),
    }
}

pub(crate) fn arg_str(call: &ToolCall, key: &str) -> Result<String, ToolFailure> {
    call.args
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            err(
                FailureClass::Error,
                format!("`{key}` is required and must be a string"),
            )
        })
}

/// A refusal's failure class. Containment is `Denied`; a missing file is `NotFound`; everything
/// else the model can fix by writing a different patch is `Error`.
fn class(e: &PatchError) -> FailureClass {
    match e {
        PatchError::Denied { .. } => FailureClass::Denied,
        PatchError::Io(m) if m.contains("no such file") => FailureClass::NotFound,
        _ => FailureClass::Error,
    }
}

/// `view` — Generic render, concurrency-safe. Returns `[path#TAG]` plus `N:text` rows and
/// remembers the text in [`SeenFiles`].
pub struct View {
    pub cfg: Arc<OperatorConfig>,
    pub root: WorkspaceRoot,
    pub seen: Arc<SeenFiles>,
}

#[async_trait::async_trait]
impl Tool for View {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let path = arg_str(&call, "path")?;
        if path.trim().is_empty() {
            return Err(err(
                FailureClass::Error,
                "view needs a path — pass one relative to the workspace, or an absolute one \
                 inside it.",
            ));
        }
        let abs = super::contain(&self.root, &path).map_err(|m| err(FailureClass::Denied, m))?;

        let stat = std::fs::metadata(&abs).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                err(
                    FailureClass::NotFound,
                    format!(
                        "cannot view {path}: no such file (looked at {}). Relative paths resolve \
                         against the workspace — check it with bash, or create it with \
                         write(\"{path}\", …).",
                        abs.display()
                    ),
                )
            } else {
                err(FailureClass::Error, format!("cannot view {path}: {e}"))
            }
        })?;
        if stat.is_dir() {
            return Err(err(
                FailureClass::Error,
                format!(
                    "cannot view {path}: it is a directory, not a file. List it with bash and \
                     view one of the files inside it."
                ),
            ));
        }
        // Refusing beats truncating: a truncated listing still carries line numbers, and the
        // model would write anchors against a version it never saw.
        if stat.len() as usize > self.cfg.max_view_bytes {
            return Err(err(
                FailureClass::Error,
                format!(
                    "cannot view {path}: it is {} bytes, over the {}-byte view limit, and \
                     rendering it would overflow the context window. Read the part you need with \
                     bash (rg -n PATTERN, sed -n '1,200p'); patch needs a view to anchor to, so \
                     edit a smaller file or rewrite this one with write.",
                    stat.len(),
                    self.cfg.max_view_bytes
                ),
            ));
        }

        let bytes = std::fs::read(&abs)
            .map_err(|e| err(FailureClass::Error, format!("cannot view {path}: {e}")))?;
        // Decoding is lossy, so a binary file arrives as replacement characters and writing it
        // back would destroy it. Refuse before it is on record.
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if text.contains('\u{0}') {
            return Err(err(
                FailureClass::Error,
                format!(
                    "cannot view {path}: it contains NUL bytes, so it is not a text file — \
                     viewing it would decode it lossily and patching it would corrupt it."
                ),
            ));
        }

        // Keyed by the RESOLVED path: "m.rs" and "./m.rs" are one file and must be one record.
        self.seen
            .remember(call.agent.clone(), abs.clone(), tag_of(&text), text.clone());

        let rendered = render_numbered(&path, &text);
        let content = if text.is_empty() {
            format!(
                "{}\n(this file is empty — use INS.HEAD: to put the first lines in, or write to \
                 replace it wholesale)",
                rendered.trim_end()
            )
        } else {
            rendered
        };
        Ok(ToolOutcome {
            content,
            value: None,
            cites: vec![super::file_cite(&abs)],
            concludes_wake: false,
        })
    }
}

/// `patch` — Diff render, not concurrency-safe.
pub struct Patch {
    pub cfg: Arc<OperatorConfig>,
    pub root: WorkspaceRoot,
    pub seen: Arc<SeenFiles>,
}

#[async_trait::async_trait]
impl Tool for Patch {
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let input = arg_str(&call, "patch")?;
        // The file bound is checked before anything is read, so an oversized patch costs no IO.
        let groups = parse_patch(&input)
            .and_then(|ops| group_by_file(&ops))
            .map_err(|e| err(class(&e), e.to_string()))?;
        if groups.len() > self.cfg.max_files_per_patch {
            return Err(err(
                FailureClass::Error,
                format!(
                    "this patch names {} files, over the limit of {}. Split it; a patch applies \
                     to all its files or none.",
                    groups.len(),
                    self.cfg.max_files_per_patch
                ),
            ));
        }
        let applied = apply_patch(&input, &self.root, &call.agent, &self.seen)
            .map_err(|e| err(class(&e), e.to_string()))?;
        let cites = applied
            .iter()
            .filter_map(|a| super::contain(&self.root, &a.path).ok())
            .map(|p| super::file_cite(&p))
            .collect();
        Ok(ToolOutcome {
            content: applied.iter().map(echo).collect::<Vec<_>>().join("\n"),
            value: None,
            cites,
            concludes_wake: false,
        })
    }
}
