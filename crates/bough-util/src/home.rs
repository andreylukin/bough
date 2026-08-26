//! Invariant: every path under bough's home is derived here, and the environment is read on EVERY
//! call — never memoised in a `OnceLock` — so a test can point one test process at one `TempDir`
//! by setting `BOUGH_HOME` (§0.5, and the Phase 0 SWAP test depends on it).

use std::path::{Path, PathBuf};

/// `$HOME`, else the platform home directory.
pub fn home_dir() -> PathBuf {
    todo!("WP-1: read $HOME, fall back to dirs::home_dir()")
}

/// `$BOUGH_HOME`, else `~/.bough`.
pub fn bough_home() -> PathBuf {
    todo!("WP-1: read $BOUGH_HOME, else home_dir().join(\".bough\")")
}

/// `bough_home().join(rel)`, normalised (no `.` / `..` components left in the result).
pub fn bough_path(rel: impl AsRef<Path>) -> PathBuf {
    let _ = rel.as_ref();
    todo!("WP-1: join and normalise")
}

/// The user patch layer the launcher stacks last and watches: `bough_path(\"bough.patch.yml\")`.
pub fn user_patch_path() -> PathBuf {
    bough_path("bough.patch.yml")
}

/// Create `p` and its parents if absent. Succeeds when it already exists.
pub fn ensure_dir(p: &Path) -> std::io::Result<()> {
    let _ = p;
    todo!("WP-1: create_dir_all, tolerating AlreadyExists")
}
