//! Per-session scratch dirs (port of `src/scratch.ts`). Scratch lives UNDER
//! `~/.bough`, NOT `/tmp` (macOS empties `/tmp` on reboot and systemd-tmpfiles
//! reaps entries older than ten days); PER SESSION (two conversations both
//! writing `/tmp/build.log` clobber each other); and so it must be swept by us
//! — age is the honest criterion, swept at boot, best-effort, never on the
//! path of a turn. Only DIRECTORIES are swept (a loose file in the root is
//! left alone — "a recursive delete of anything it finds is how a bug here
//! becomes data loss").

use std::path::PathBuf;

use crate::paths::{scratch_dir_for, scratch_root};

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
/// MTIME OF THE DIRECTORY, not of the session row: a conversation can be
/// months old and still be the one you are working in, and the question here is
/// whether anything has been WRITTEN lately.
pub fn sweep_scratch(opts: SweepOptions) -> Vec<String> {
    let root = opts.root.unwrap_or_else(scratch_root);
    let now = opts.now.unwrap_or_else(now_ms);
    let max_age = opts.max_age_ms.unwrap_or(MAX_AGE_MS);
    // No root yet: nothing has ever been written, nothing to sweep. Not an
    // error — the server must start on a machine that has never scratched.
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        // A directory that vanished under us, or one we may not read. Either
        // way it is not this sweep's business to complain about.
        let Ok(meta) = std::fs::metadata(&dir) else {
            continue;
        };
        // A loose FILE in the root is left alone: the sweep's rule is about
        // directories it created, and a recursive delete of anything it finds
        // is how a bug here becomes data loss.
        if !meta.is_dir() {
            continue;
        }
        let Some(mtime) = mtime_ms(&meta) else {
            continue;
        };
        if now - mtime <= max_age {
            continue;
        }
        if std::fs::remove_dir_all(&dir).is_ok() {
            removed.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    removed
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Epoch-ms mtime, or `None` when the platform will not say — an unknown age is
/// not a stale one, so the directory is kept.
fn mtime_ms(meta: &std::fs::Metadata) -> Option<i64> {
    let modified = meta.modified().ok()?;
    match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => Some(d.as_millis() as i64),
        // Before the epoch: as stale as it gets.
        Err(e) => Some(-(e.duration().as_millis() as i64)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_env::with_env;
    use std::path::Path;

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

    /// A scratch directory with one file in it, last touched `age_ms` ago.
    fn aged(root: &Path, name: &str, age_ms: i64, now: i64) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("note.txt"), "x").unwrap();
        set_mtime(&dir, now - age_ms);
        dir
    }

    fn set_mtime(path: &PathBuf, ms: i64) {
        // No `std` setter for mtime; `touch -t`-style via libc utimes is what
        // the platform offers, and the test needs an exact instant.
        let secs = ms.div_euclid(1000);
        let usecs = ms.rem_euclid(1000) * 1000;
        let tv = libc::timeval {
            tv_sec: secs as libc::time_t,
            tv_usec: usecs as libc::suseconds_t,
        };
        let times = [tv, tv];
        let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        let rc = unsafe { libc::utimes(c.as_ptr(), times.as_ptr()) };
        assert_eq!(rc, 0, "utimes failed for {path:?}");
    }

    // scratch.test.ts: "the sweep removes what is stale and keeps what is not"
    #[test]
    fn sweep_removes_what_is_stale_and_keeps_what_is_not() {
        // 2026-07-29T12:00:00Z
        let now = 1_785_326_400_000i64;
        let root = temp_root("sweep-stale");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let fresh = aged(&root, "fresh-session", 60_000, now);
        let yesterday = aged(&root, "yesterday", 24 * 60 * 60_000, now);
        let ancient = aged(&root, "ancient", MAX_AGE_MS + 60_000, now);

        let removed = sweep_scratch(SweepOptions {
            root: Some(root.clone()),
            now: Some(now),
            max_age_ms: None,
        });
        assert_eq!(removed, vec!["ancient".to_string()]);
        assert!(!ancient.exists());
        // A conversation can be months old and still be the one you are working
        // in, so the question is when anything was last WRITTEN.
        assert!(fresh.is_dir());
        assert!(yesterday.is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    // The boundary itself: `now - mtime <= max_age` KEEPS.
    #[test]
    fn exactly_max_age_is_kept_and_one_ms_older_goes() {
        let now = 1_785_326_400_000i64;
        let root = temp_root("sweep-boundary");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let edge = aged(&root, "edge", MAX_AGE_MS, now);
        let over = aged(&root, "over", MAX_AGE_MS + 1, now);

        let removed = sweep_scratch(SweepOptions {
            root: Some(root.clone()),
            now: Some(now),
            max_age_ms: None,
        });
        assert_eq!(removed, vec!["over".to_string()]);
        assert!(edge.is_dir(), "exactly at the age is not yet stale");
        assert!(!over.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    // scratch.test.ts: "a missing root is not an error"
    #[test]
    fn a_missing_root_is_not_an_error() {
        let root = temp_root("sweep-missing").join("never-created");
        let _ = std::fs::remove_dir_all(&root);
        assert!(sweep_scratch(SweepOptions {
            root: Some(root),
            ..Default::default()
        })
        .is_empty());
    }

    // scratch.test.ts: "a file loose in the root is left alone"
    #[test]
    fn a_file_loose_in_the_root_is_left_alone() {
        // Not ours to delete: a recursive delete of anything it finds is how a
        // bug here becomes data loss.
        let now = 1_785_326_400_000i64;
        let root = temp_root("sweep-stray");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("stray.txt"), "x").unwrap();
        set_mtime(&root.join("stray.txt"), now - MAX_AGE_MS * 4);
        let removed = sweep_scratch(SweepOptions {
            root: Some(root.clone()),
            now: Some(now + MAX_AGE_MS * 2),
            max_age_ms: None,
        });
        assert!(removed.is_empty());
        assert!(root.join("stray.txt").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }
}
