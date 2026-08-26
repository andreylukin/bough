//! Invariant (§7): `root` is a CONTAINMENT CHECK, not a sandbox. It exists so a worker's relative
//! path cannot leave the task tree by accident; it is not a security boundary and must never be
//! described as one.

use std::path::{Path, PathBuf};

/// Resolve `path` against `root`, refusing anything that escapes it (`..`, an absolute path
/// elsewhere, a symlink out).
///
/// WP-3.
pub fn contain(_root: &Path, _path: &str) -> Result<PathBuf, String> {
    todo!("WP-3: canonicalise and refuse an escape, naming the root in the message")
}

/// Whether `path` matches any of the row's `deny_globs`. WP-3.
pub fn denied(_deny_globs: &[String], _path: &Path) -> bool {
    todo!("WP-3")
}
