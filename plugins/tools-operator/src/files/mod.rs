//! The three file verbs and main's hash-anchored patch grammar. `edit_file(old, new)` is a
//! regression and is not offered here.

pub mod apply;
pub mod grammar;
pub mod rebase;
pub mod seen;
pub mod view;
pub mod write;

use std::sync::Arc;

use bough_plugin_tools::ToolSpec;

pub use apply::{apply_patch, Applied};
pub use grammar::{
    check_ops, group_by_file, materialize, normalize, parse_patch, render_numbered, tag_of,
    FileOps, OpKind, PatchError, PatchOp,
};
pub use rebase::{line_map, rebase_ops, RebaseConflict, RebaseResult};
pub use seen::SeenFiles;

/// The `view`/`patch`/`write` specs. WP-4's `lib.rs` registers them alongside the other four.
///
/// WP-3 owns the body.
pub fn specs(_cfg: Arc<crate::OperatorConfig>, _seen: Arc<SeenFiles>) -> Vec<ToolSpec> {
    todo!("WP-3: the three file-verb ToolSpecs")
}
