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
    pub fn new(p: PathBuf) -> WorkspaceRoot {
        WorkspaceRoot(Arc::new(p))
    }

    pub fn path(&self) -> &Path {
        self.0.as_path()
    }
}
