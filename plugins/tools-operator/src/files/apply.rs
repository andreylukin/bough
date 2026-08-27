//! Invariant: one patch may carry several files and applies ALL of them or NONE. A conflict in
//! one file leaves every other file byte-identical.

use super::grammar::PatchError;
use super::seen::SeenFiles;

/// What one file's application produced — echoed back so the next patch can chain onto the tag
/// without a re-view.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    pub path: String,
    /// The file's tag AFTER the patch.
    pub tag: String,
    pub lines_added: usize,
    pub lines_removed: usize,
}

/// Parse, rebase, check and apply a whole patch, atomically across files.
///
/// WP-3 owns the body.
pub fn apply_patch(_input: &str, _seen: &SeenFiles) -> Result<Vec<Applied>, PatchError> {
    todo!("WP-3: parse → group → rebase → check → materialize → write, all-or-nothing")
}
