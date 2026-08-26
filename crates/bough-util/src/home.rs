//! Invariant: every path under bough's home is derived here, and the environment is read on EVERY
//! call — never memoised in a `OnceLock` — so a test can point one test process at one `TempDir`
//! by setting `BOUGH_HOME` (§0.5, and the Phase 0 SWAP test depends on it).

use std::path::{Component, Path, PathBuf};

/// `$HOME`, else the platform home directory.
pub fn home_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(h) if !h.is_empty() => PathBuf::from(h),
        _ => dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
    }
}

/// `$BOUGH_HOME`, else `~/.bough`.
pub fn bough_home() -> PathBuf {
    match std::env::var_os("BOUGH_HOME") {
        Some(h) if !h.is_empty() => normalise(Path::new(&h)),
        _ => normalise(&home_dir().join(".bough")),
    }
}

/// `bough_home().join(rel)`, normalised (no `.` / `..` components left in the result).
pub fn bough_path(rel: impl AsRef<Path>) -> PathBuf {
    normalise(&bough_home().join(rel))
}

/// The user patch layer the launcher stacks last and watches: `bough_path("bough.patch.yml")`.
pub fn user_patch_path() -> PathBuf {
    bough_path("bough.patch.yml")
}

/// Create `p` and its parents if absent. Succeeds when it already exists.
pub fn ensure_dir(p: &Path) -> std::io::Result<()> {
    match std::fs::create_dir_all(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Lexical normalisation: drop `.`, pop a component for `..` when there is one to pop. Purely
/// textual on purpose — it must not touch the filesystem or resolve symlinks.
fn normalise(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// The process environment is global; these tests set it, so they take a lock rather than
    /// racing each other under the default multi-threaded test harness.
    fn env_lock() -> MutexGuard<'static, ()> {
        static L: OnceLock<Mutex<()>> = OnceLock::new();
        L.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    struct EnvGuard {
        home: Option<std::ffi::OsString>,
        bough_home: Option<std::ffi::OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn take() -> Self {
            Self {
                home: std::env::var_os("HOME"),
                bough_home: std::env::var_os("BOUGH_HOME"),
                _lock: env_lock(),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            restore("HOME", self.home.take());
            restore("BOUGH_HOME", self.bough_home.take());
        }
    }

    fn restore(k: &str, v: Option<std::ffi::OsString>) {
        match v {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    }

    #[test]
    fn bough_home_honours_env() {
        let _g = EnvGuard::take();
        std::env::set_var("BOUGH_HOME", "/tmp/bough-util-test-home");
        assert_eq!(bough_home(), PathBuf::from("/tmp/bough-util-test-home"));
    }

    #[test]
    fn bough_home_defaults_under_home() {
        let _g = EnvGuard::take();
        std::env::remove_var("BOUGH_HOME");
        std::env::set_var("HOME", "/tmp/bough-util-test-user");
        assert_eq!(
            bough_home(),
            PathBuf::from("/tmp/bough-util-test-user/.bough")
        );
    }

    #[test]
    fn user_patch_path_is_under_bough_home() {
        let _g = EnvGuard::take();
        std::env::set_var("BOUGH_HOME", "/tmp/bough-util-test-patch");
        assert_eq!(
            user_patch_path(),
            PathBuf::from("/tmp/bough-util-test-patch/bough.patch.yml")
        );
    }

    #[test]
    fn bough_path_is_absolute() {
        let _g = EnvGuard::take();
        std::env::set_var("BOUGH_HOME", "/tmp/bough-util-test-abs/./sub/..");
        let p = bough_path("plugins/x.yml");
        assert!(p.is_absolute(), "{p:?} is not absolute");
        assert_eq!(p, PathBuf::from("/tmp/bough-util-test-abs/plugins/x.yml"));
    }

    #[test]
    fn ensure_dir_is_idempotent() {
        let dir = std::env::temp_dir().join("bough-util-ensure-dir/a/b");
        ensure_dir(&dir).unwrap();
        ensure_dir(&dir).unwrap();
        assert!(dir.is_dir());
    }
}
