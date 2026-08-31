//! The three file verbs and main's hash-anchored patch grammar. `edit_file(old, new)` is a
//! regression and is not offered here.
//!
//! Invariant: every path a verb touches is resolved through [`contain`] against the pinned
//! `ctx.workspace` root — a containment CHECK, not a sandbox — so a relative path cannot leave the
//! tree by accident and an absolute path elsewhere is refused as `Denied`.

pub mod apply;
pub mod grammar;
pub mod rebase;
pub mod seen;
pub mod view;
pub mod write;

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use bough_plugin_tools::{RenderIntent, ToolName, ToolScope, ToolSpec, WorkspaceRoot};

pub use apply::{apply_patch, Applied};
pub use grammar::{
    check_ops, group_by_file, join_lines, materialize, normalize, parse_patch, render_numbered,
    tag_of, to_lines, FileOps, OpKind, PatchError, PatchOp,
};
pub use rebase::{line_map, rebase_ops, RebaseConflict, RebaseResult};
pub use seen::SeenFiles;

/// Resolve `path` against the pinned workspace root, refusing anything that escapes it (`..`, an
/// absolute path elsewhere, a symlink out).
///
/// The target need not exist (a `write` creates it), so the deepest EXISTING ancestor is
/// canonicalised and the rest appended lexically. The root is already absolute and canonical
/// ([`WorkspaceRoot`] enforces it), so it is never canonicalised again here — doing that per call
/// is what let a later `chdir` retarget every tool (phase ux1 §2.10, B5).
pub fn contain(root: &WorkspaceRoot, path: &str) -> Result<PathBuf, String> {
    let root = root.path().to_path_buf();
    let joined = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        root.join(path)
    };
    let resolved = resolve_existing_prefix(&normalize_lexical(&joined));
    if !resolved.starts_with(&root) {
        return Err(format!(
            "path `{path}` is outside the workspace `{}`",
            root.display()
        ));
    }
    Ok(resolved)
}

/// Lexical normalisation: drop `.`, fold `..` against the preceding component. No filesystem
/// access, so it works for paths that do not exist yet.
fn normalize_lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonicalise the deepest existing ancestor and re-append the rest, so a symlinked ancestor
/// (`/var` → `/private/var` on macOS, and a link out of the tree) is resolved before the
/// containment comparison.
fn resolve_existing_prefix(p: &Path) -> PathBuf {
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        if let Ok(c) = cur.canonicalize() {
            let mut out = c;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match cur.file_name() {
            Some(name) => {
                rest.push(name.to_os_string());
                if !cur.pop() {
                    return p.to_path_buf();
                }
            }
            None => return p.to_path_buf(),
        }
    }
}

/// A file cite, so a `view` result is EVIDENCE rather than a thought (P2-D26).
pub(crate) fn file_cite(p: &Path) -> bough_plugin_ledger::Cite {
    bough_plugin_ledger::Cite {
        r#ref: bough_plugin_ledger::Ref::new(format!("file:{}", p.display())),
        url: None,
    }
}

fn schema(v: serde_json::Value) -> schemars::Schema {
    schemars::Schema::try_from(v).expect("a file verb's input schema is an object")
}

/// The `view`/`patch`/`write` specs. WP-4's `lib.rs` registers them alongside the other four.
///
/// The `root` argument is the pinned `ctx.workspace` value: the plan's signature took only
/// `(cfg, seen)`, but nothing in `OperatorConfig` names a directory and the verbs must resolve
/// against the SAME root the rest of the tools do (see the report in the merge notes).
pub fn specs(
    cfg: Arc<crate::OperatorConfig>,
    root: WorkspaceRoot,
    seen: Arc<SeenFiles>,
) -> Vec<ToolSpec> {
    let string = serde_json::json!({ "type": "string" });
    vec![
        ToolSpec {
            name: ToolName::new("view"),
            description: "Read a file as `[path#TAG]` plus numbered lines. The TAG and the line \
                          numbers are what `patch` anchors to."
                .into(),
            input_schema: schema(serde_json::json!({
                "type": "object",
                "properties": { "path": string },
                "required": ["path"]
            })),
            render: RenderIntent::Generic,
            scope: ToolScope::Global,
            tool: Arc::new(view::View {
                cfg: cfg.clone(),
                root: root.clone(),
                seen: seen.clone(),
            }),
        },
        ToolSpec {
            name: ToolName::new("patch"),
            description: "Apply hash-anchored line edits: `[path#TAG]` sections of SWAP A.=B:, \
                          DEL A.=B, INS.PRE A:, INS.POST A:, INS.HEAD:, INS.TAIL: with `+` body \
                          rows. All files or none."
                .into(),
            input_schema: schema(serde_json::json!({
                "type": "object",
                "properties": { "patch": string },
                "required": ["patch"]
            })),
            render: RenderIntent::Diff,
            scope: ToolScope::Global,
            tool: Arc::new(view::Patch {
                cfg: cfg.clone(),
                root: root.clone(),
                seen: seen.clone(),
            }),
        },
        ToolSpec {
            name: ToolName::new("write"),
            description: "Create a file or replace one wholesale, creating parent directories. \
                          Echoes the new TAG, so the next `patch` can anchor to it without a \
                          re-view."
                .into(),
            input_schema: schema(serde_json::json!({
                "type": "object",
                "properties": { "path": string, "content": string },
                "required": ["path", "content"]
            })),
            render: RenderIntent::Diff,
            scope: ToolScope::Global,
            tool: Arc::new(write::Write { cfg, root, seen }),
        },
    ]
}
