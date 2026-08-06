//! Per-session scratch dirs (port of `src/scratch.ts`). Scratch lives UNDER
//! `~/.bough`, NOT `/tmp` (macOS empties `/tmp` on reboot and systemd-tmpfiles
//! reaps entries older than ten days); PER SESSION (two conversations both
//! writing `/tmp/build.log` clobber each other); and so it must be swept by us
//! — age is the honest criterion, swept at boot, best-effort, never on the
//! path of a turn. Only DIRECTORIES are swept (a loose file in the root is
//! left alone — "a recursive delete of anything it finds is how a bug here
//! becomes data loss").

use std::path::PathBuf;

use crate::paths::scratch_dir_for;

/// How long an untouched scratch directory is kept. Two weeks: long enough to
/// span a holiday, short enough that the root does not become an archive.
pub const MAX_AGE_MS: i64 = 14 * 24 * 60 * 60_000;

/// This session's scratch directory, created if it is not there.
///
/// Called before the prompt names the path; the dir is exported to every shell
/// command as `$BOUGH_SCRATCH` and used as the output-spill target.
///
/// **Never fails.** An unwritable `~/.bough` is a real problem but not this
/// function's to raise — the turn can still run, and every other write path
/// reports its own failure in terms the reader can act on. Always returns the
/// path.
pub fn ensure_scratch_dir(session_id: &str) -> PathBuf {
    let dir = scratch_dir_for(session_id);
    // Reported by whatever writes next, in its own terms.
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[derive(Default)]
pub struct SweepOptions {
    /// Absent = [`MAX_AGE_MS`].
    pub max_age_ms: Option<i64>,
    /// Injected clock, epoch ms.
    pub now: Option<i64>,
    /// Absent = the real root. Tests pass their own.
    pub root: Option<PathBuf>,
}

/// Delete scratch DIRECTORIES whose dir mtime is strictly older than the max
/// age (`now - mtime <= max_age` keeps); returns removed names. Missing root →
/// `[]`, not an error. Criterion is dir MTIME, never the session row.
///
/// v1 STUB (PORT_PLAN row 1.25): the sweep is a pinned no-op — a root that
/// grows for a few weeks is harmless; `ensure` is the load-bearing half.
pub fn sweep_scratch(_opts: SweepOptions) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_env::with_env;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("bough-scratch-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn ensure_creates_the_session_dir_and_returns_it() {
        let root = temp_root("create");
        let _ = std::fs::remove_dir_all(&root);
        with_env(&[("BOUGH_HOME", Some(root.to_str().unwrap()))], || {
            let dir = ensure_scratch_dir("s1");
            assert_eq!(dir, root.join("scratch").join("s1"));
            assert!(dir.is_dir(), "{dir:?} was not created");
            // Idempotent: a second call is a no-op, same path.
            assert_eq!(ensure_scratch_dir("s1"), dir);
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ensure_never_fails_even_when_mkdir_cannot_succeed() {
        // The "root" is a regular FILE, so create_dir_all must fail — and the
        // function still returns the path. The next write path reports.
        let root = temp_root("blocked");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&root);
        std::fs::write(&root, "not a directory").unwrap();
        with_env(&[("BOUGH_HOME", Some(root.to_str().unwrap()))], || {
            let dir = ensure_scratch_dir("s1");
            assert_eq!(dir, root.join("scratch").join("s1"));
            assert!(!dir.exists());
        });
        let _ = std::fs::remove_file(&root);
    }

    #[test]
    fn sweep_is_a_pinned_no_op_in_v1() {
        // Even a root full of ancient directories is left alone until the
        // sweep is ported (PORT_PLAN 3.22).
        let root = temp_root("sweep");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("ancient-session")).unwrap();
        let removed = sweep_scratch(SweepOptions {
            max_age_ms: Some(0),
            now: Some(i64::MAX),
            root: Some(root.clone()),
        });
        assert!(removed.is_empty());
        assert!(root.join("ancient-session").is_dir(), "the stub must not delete");
        assert!(sweep_scratch(SweepOptions::default()).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
