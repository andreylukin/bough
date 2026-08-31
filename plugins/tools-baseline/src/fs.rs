//! Invariant (§7): `root` is a CONTAINMENT CHECK, not a sandbox. It exists so a worker's relative
//! path cannot leave the task tree by accident; it is not a security boundary and must never be
//! described as one.

use std::path::{Component, Path, PathBuf};

/// Resolve `path` against `root`, refusing anything that escapes it (`..`, an absolute path
/// elsewhere, a symlink out).
///
/// The target need not exist (a write creates it), so the deepest EXISTING ancestor is
/// canonicalised and the rest is appended lexically: that is what makes a symlink pointing out of
/// the tree refusable without requiring the leaf to be there already.
///
/// `root` is the PINNED root — absolute and already canonicalised by [`pin_root`] at activation.
/// This function never canonicalises it again: doing so on every call is exactly what let a later
/// `chdir` retarget every tool (phase ux1 §2.10, B5).
pub fn contain(root: &Path, path: &str) -> Result<PathBuf, String> {
    let root = root.to_path_buf();
    let joined = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        root.join(path)
    };
    let resolved = resolve_existing_prefix(&normalize(&joined));
    if !resolved.starts_with(&root) {
        return Err(format!(
            "path `{path}` is outside the tool root `{}`",
            root.display()
        ));
    }
    Ok(resolved)
}

/// Lexical normalisation: drop `.`, fold `..` against the preceding component. No filesystem
/// access, so it works for paths that do not exist yet.
fn normalize(p: &Path) -> PathBuf {
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

/// Whether `path` matches any of the row's `deny_globs`.
///
/// `**` crosses separators, `*` and `?` do not. The pattern is matched against the whole path and
/// against its tail after any `/`, so `*.env` denies `a/b/.env` too.
pub fn denied(deny_globs: &[String], path: &Path) -> bool {
    let s = path.to_string_lossy().to_string();
    deny_globs.iter().any(|g| {
        let re = glob_to_regex(g);
        regex::Regex::new(&re)
            .map(|r| {
                r.is_match(&s)
                    || path
                        .file_name()
                        .map(|n| r.is_match(&n.to_string_lossy()))
                        .unwrap_or(false)
            })
            .unwrap_or(false)
    })
}

/// A glob as an anchored regex. Shared by `denied` and by the `glob` tool.
pub fn glob_to_regex(pattern: &str) -> String {
    let mut re = String::from("^");
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    // `**/` also matches zero directories.
                    if i + 2 < chars.len() && chars[i + 2] == '/' {
                        re.push_str("(?:.*/)?");
                        i += 3;
                        continue;
                    }
                    re.push_str(".*");
                    i += 2;
                    continue;
                }
                re.push_str("[^/]*");
            }
            '?' => re.push_str("[^/]"),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    re.push('$');
    re
}

/// PURE: a configured root against the process cwd, resolved ONCE at activation (phase ux1
/// §2.10, B5). A relative root joins the cwd; an absolute one is taken as given; the result is
/// canonicalised, and a root that does not exist is a LOAD failure, not a per-call error
/// (§0.2 fail loud).
///
/// `contain` then takes this ABSOLUTE root and never canonicalises again — the per-call
/// canonicalisation is exactly what let a later `chdir` retarget every tool.
pub fn pin_root(configured: &Path, process_cwd: &Path) -> Result<PathBuf, String> {
    let joined = if configured.is_absolute() {
        configured.to_path_buf()
    } else {
        process_cwd.join(configured)
    };
    let normalized = normalize(&joined);
    normalized.canonicalize().map_err(|e| {
        format!(
            "tool root `{}` (resolved to `{}`) is unreadable: {e}",
            configured.display(),
            normalized.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The root as a row actually holds it: pinned once, absolute and canonical. `TempDir::path`
    /// is NOT canonical on macOS (`/var` -> `/private/var`), and `contain` no longer canonicalises
    /// per call, so every test resolves its root the way activation does.
    fn root(dir: &tempfile::TempDir) -> PathBuf {
        pin_root(Path::new("."), dir.path()).unwrap()
    }

    #[test]
    fn a_relative_path_resolves_under_the_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let p = contain(&root(&dir), "a.txt").unwrap();
        assert!(p.ends_with("a.txt"));
        assert_eq!(std::fs::read_to_string(p).unwrap(), "hi");
    }

    #[test]
    fn a_path_that_does_not_exist_yet_still_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let p = contain(&root(&dir), "sub/new.txt").unwrap();
        assert!(p.ends_with("sub/new.txt"));
    }

    #[test]
    fn a_dotdot_escape_is_refused_and_names_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let err = contain(&root(&dir), "../outside.txt").unwrap_err();
        assert!(err.contains("outside the tool root"), "{err}");
    }

    #[test]
    fn an_absolute_path_elsewhere_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(contain(&root(&dir), "/etc/hosts").is_err());
    }

    #[test]
    fn deny_globs_match_the_tail_and_the_whole_path() {
        let p = PathBuf::from("/root/a/b/.env");
        assert!(denied(&["*.env".to_string()], &p));
        assert!(denied(&["**/b/*".to_string()], &p));
        assert!(!denied(&["*.toml".to_string()], &p));
    }

    #[test]
    fn a_double_star_prefix_matches_zero_directories() {
        let re = regex::Regex::new(&glob_to_regex("**/*.rs")).unwrap();
        assert!(re.is_match("main.rs"));
        assert!(re.is_match("src/a/main.rs"));
        assert!(!re.is_match("main.toml"));
    }
}
