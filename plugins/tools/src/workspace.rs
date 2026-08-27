//! Invariant: there is ONE directory tool calls resolve against, it is pinned at boot, and it is
//! PUBLISHED — so the value the tools use and the value the status line shows are the same object
//! and a divergence is impossible rather than untested (phase ux1 §2.10, B5).
//!
//! This crate is the Service DEFINITION: it declares the key and the type. The Provider is
//! `tools-baseline`, which pins the root once at activation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bough_kernel::ServiceKey;

/// `ctx.workspace` — the one directory tool calls resolve against, pinned at boot.
pub struct Workspace;

impl ServiceKey for Workspace {
    type Value = WorkspaceRoot;
    const NAME: &'static str = "workspace";
}

/// An ABSOLUTE, canonicalised directory. Constructing one is the only way to name the root, so a
/// relative path can never reach a tool call.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceRoot(Arc<PathBuf>);

impl WorkspaceRoot {
    /// The ONLY constructor, and it ENFORCES the type's invariant instead of documenting it.
    /// `new` used to take any `PathBuf`, which left "absolute and canonical" resting on the single
    /// call site in `tools-baseline` — and `fs::contain` now trusts absoluteness without
    /// re-canonicalising, so a root built anywhere else would silently reintroduce B5.
    pub fn new(p: PathBuf) -> Result<WorkspaceRoot, String> {
        if !p.is_absolute() {
            return Err(format!(
                "the workspace root must be an ABSOLUTE path; got {}",
                p.display()
            ));
        }
        if p.components().any(|c| {
            matches!(
                c,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        }) {
            return Err(format!(
                "the workspace root must be CANONICAL: {} still contains `.` or `..`",
                p.display()
            ));
        }
        Ok(WorkspaceRoot(Arc::new(p)))
    }

    pub fn path(&self) -> &Path {
        self.0.as_path()
    }
}
