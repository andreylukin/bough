//! The `~/.bough` data-root layout (port of `src/paths.ts`).
//!
//! The invariant: **no module builds a `~/.bough` path by string
//! concatenation.** Every subpath has a named accessor here. `BOUGH_HOME`
//! overrides the root — it is what lets the rewrite run beside the live
//! install. Env vars are read PER CALL — do not cache the root; tests flip the
//! env var per call.
//!
//! `confine` is purely lexical: nothing is stat'd, no symlink followed. Do NOT
//! use `std::fs::canonicalize` (it follows symlinks and requires existence).

use std::path::{Component, Path, PathBuf};

use crate::errors::BoughError;

/// `$BOUGH_HOME` if set and non-blank (whitespace-only counts as unset — "a
/// shell accident, not a request to put the data root at the cwd"), else
/// `~/.bough`.
pub fn bough_home() -> PathBuf {
    match std::env::var("BOUGH_HOME") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".bough"),
    }
}

/// Join `segs` under the data root.
pub fn bough_path(segs: &[&str]) -> PathBuf {
    let mut p = bough_home();
    for s in segs {
        p.push(s);
    }
    p
}

/// The SQLite database. `$BOUGH_DB` overrides it OUTRIGHT if set (`:memory:`
/// legal — TS `??` semantics: any set value wins), else `<home>/bough.db`.
pub fn db_path() -> PathBuf {
    match std::env::var("BOUGH_DB") {
        Ok(v) => PathBuf::from(v),
        _ => bough_path(&["bough.db"]),
    }
}

/// Filesystem is source of truth for artifacts; survives a DB reset.
pub fn artifacts_dir() -> PathBuf {
    bough_path(&["artifacts"])
}

pub fn artifacts_dir_for(session_id: &str) -> PathBuf {
    artifacts_dir().join(session_id)
}

/// The hooks you wrote: `~/.bough/hooks`. Bundled and cloned hooks live
/// elsewhere — see `hooks::sources`.
pub fn hooks_dir() -> PathBuf {
    bough_path(&["hooks"])
}

/// Where bough's own plugins live: `~/.bough/plugins/<name>/`.
///
/// A PLUGIN IS A DIRECTORY, AND THAT IS THE WHOLE POINT. The three extension
/// surfaces each had their own flat drop-box — a `.lua` in `hooks/`, a `.js`
/// in `extensions/`, a folder in `skills/` — so one coherent thing shipping
/// all three arrived as three unrelated files with no shared name, no way to
/// install or remove it in one move, and nothing saying which file came with
/// which. A plugin is one directory holding `hooks/`, `skills/` and
/// `extensions/`; the DIRECTORY NAME is its identity, which is what makes a
/// hook inside it addressable as `<plugin>/<file>.lua`.
///
/// The flat drop-boxes are unchanged and still first-class: they are the
/// files YOU wrote for yourself, and demanding a directory for a ten-line
/// hook would be ceremony.
pub fn plugins_dir() -> PathBuf {
    bough_path(&["plugins"])
}

/// Every plugin directory, name-sorted, so every surface enumerates them in
/// the same order.
pub fn plugin_dirs() -> Vec<PathBuf> {
    plugin_dirs_in(&plugins_dir())
}

/// [`plugin_dirs`] against a given root — tests point it somewhere temporary
/// rather than redirecting `BOUGH_HOME`.
pub fn plugin_dirs_in(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        // A dotfile directory is bookkeeping (`.git` in a cloned plugin
        // collection), never a plugin.
        .filter(|p| {
            !p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
        })
        .collect();
    out.sort();
    out
}

/// Every superseded version of every artifact:
/// `artifact-versions/<sessionId>/<name>/<ts>`.
///
/// OUTSIDE `artifacts/` for the same reason the comments sidecar is — a
/// history kept inside the artifact directory would be walked by
/// `list_artifacts`, served by `GET /artifacts/:id/*`, and overwritable by a
/// program that published under the right name. Out here it is reachable only
/// through the version verbs.
pub fn artifact_versions_dir() -> PathBuf {
    bough_path(&["artifact-versions"])
}

/// Deliberately OUTSIDE `artifacts/`: a sidecar inside the artifact directory
/// would show up in every listing and be served as an artifact itself.
pub fn comments_dir() -> PathBuf {
    bough_path(&["comments"])
}

pub fn comments_path_for(session_id: &str) -> PathBuf {
    comments_dir().join(format!("{session_id}.json"))
}

/// Image bytes for `image` parts.
pub fn attachments_dir() -> PathBuf {
    bough_path(&["attachments"])
}

pub fn scratch_root() -> PathBuf {
    bough_path(&["scratch"])
}

pub fn scratch_dir_for(session_id: &str) -> PathBuf {
    scratch_root().join(session_id)
}

pub fn workflows_dir() -> PathBuf {
    bough_path(&["workflows"])
}

pub fn workflow_script_path(run_id: &str) -> PathBuf {
    workflows_dir().join(format!("{run_id}.js"))
}

pub fn maps_dir() -> PathBuf {
    bough_path(&["maps"])
}

/// The ONE accessor that confines, because `effort` is model-authored
/// (`../theme.json` must throw `PathError`).
pub fn map_dir_for(effort: &str) -> Result<PathBuf, BoughError> {
    confine(&maps_dir(), Path::new(effort))
}

pub fn user_skills_dir() -> PathBuf {
    bough_path(&["skills"])
}

pub fn theme_path() -> PathBuf {
    bough_path(&["theme.json"])
}

pub fn model_settings_path() -> PathBuf {
    bough_path(&["model.json"])
}

pub fn env_path() -> PathBuf {
    bough_path(&["env"])
}

pub fn mcp_registry_path() -> PathBuf {
    bough_path(&["mcp.json"])
}

pub fn mcp_auth_path() -> PathBuf {
    bough_path(&["mcp-auth.json"])
}

pub fn logs_dir() -> PathBuf {
    bough_path(&["logs"])
}

// ---- confinement ------------------------------------------------------------

fn has_nul(p: &Path) -> bool {
    p.as_os_str().as_encoded_bytes().contains(&0)
}

/// `JSON.stringify` for a path — the TS messages quote the inputs this way, and
/// the quoting is part of the (product-surface) message text.
fn json_quote(p: &Path) -> String {
    serde_json::to_string(&p.to_string_lossy()).unwrap_or_else(|_| format!("{:?}", p))
}

/// Purely lexical resolve: `.` skipped, `..` pops, an absolute candidate
/// restarts. Never touches the filesystem.
fn lexical_resolve(base: &Path, candidate: &Path) -> PathBuf {
    let start: PathBuf = if candidate.is_absolute() {
        PathBuf::new()
    } else {
        lexical_normalize(base)
    };
    let mut out = start;
    push_normalized(&mut out, candidate);
    out
}

fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    push_normalized(&mut out, p);
    out
}

fn push_normalized(out: &mut PathBuf, p: &Path) {
    for c in p.components() {
        match c {
            Component::Prefix(pre) => {
                *out = PathBuf::from(pre.as_os_str());
            }
            Component::RootDir => {
                if out.as_os_str().is_empty() {
                    out.push(Component::RootDir.as_os_str());
                } else {
                    *out = PathBuf::from(Component::RootDir.as_os_str());
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
                if out.as_os_str().is_empty() {
                    out.push(Component::RootDir.as_os_str());
                }
            }
            Component::Normal(seg) => out.push(seg),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(Component::RootDir.as_os_str());
    }
}

/// Resolve `candidate` against `root` and return the absolute path, or fail if
/// it escapes.
///
/// Contract (port of `paths.ts confine`):
///   - Returns an absolute, normalized path that is `root` or strictly beneath it.
///   - `PathError` (400) on `..` traversal, on an absolute `candidate` outside
///     `root`, and on a resolved path that leaves `root` by any other route.
///   - The check is on the RESOLVED path, so a chain of segments that
///     individually look harmless but resolve outward is still rejected.
///
/// **Purely lexical.** Nothing is stat'd and no symlink is followed — a real
/// symlink inside `root` pointing outward is accepted; traversal routed
/// *through* it still collapses lexically and is rejected. `root` and
/// `candidate` must be in the same lexical namespace (macOS `/tmp` vs
/// `/private/tmp`); callers build both from `bough_path()`. A relative root is
/// resolved against the cwd. NUL bytes in root OR candidate are rejected
/// before the OS sees them.
pub fn confine(root: &Path, candidate: &Path) -> Result<PathBuf, BoughError> {
    // A NUL byte truncates a path at the syscall boundary, so `a\0../../etc`
    // could pass a lexical check and then name something else entirely.
    if has_nul(root) || has_nul(candidate) {
        return Err(BoughError::path(format!(
            "path contains a NUL byte: {} under {}. Pass a plain path with no control characters.",
            json_quote(candidate),
            json_quote(root),
        )));
    }
    let base = if root.is_absolute() {
        lexical_normalize(root)
    } else {
        lexical_normalize(
            &std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("/"))
                .join(root),
        )
    };
    let full = lexical_resolve(&base, candidate);
    // `base + sep` is what makes `/a/bc` fail against root `/a/b`: a shared
    // string prefix is not containment. The `ends_with` guard keeps a
    // filesystem root ("/") from becoming "//".
    let base_bytes = base.as_os_str().as_encoded_bytes();
    let full_bytes = full.as_os_str().as_encoded_bytes();
    let sep = std::path::MAIN_SEPARATOR as u8;
    let inside = full == base || {
        let mut prefix = base_bytes.to_vec();
        if !prefix.ends_with(&[sep]) {
            prefix.push(sep);
        }
        full_bytes.starts_with(&prefix)
    };
    if inside {
        Ok(full)
    } else {
        Err(BoughError::path(format!(
            "path escapes its root: {} resolves to {}, which is outside {}. \
             Use a path that stays under {} — \"..\" segments and absolute paths \
             outside the root are rejected.",
            json_quote(candidate),
            full.display(),
            base.display(),
            base.display(),
        )))
    }
}

// ---- test support ------------------------------------------------------------

/// Serialize env-mutating tests (env vars are process-global and cargo runs
/// tests in parallel threads). Shared with `scratch.rs` tests.
#[cfg(test)]
pub(crate) mod test_env {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn lock() -> MutexGuard<'static, ()> {
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Restores the prior values on drop, even if the test body panics.
    struct Restore(Vec<(String, Option<String>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (k, v) in &self.0 {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// Run `f` with env vars set to fixed values, then restore whatever was
    /// there. `None` = unset for the duration.
    pub(crate) fn with_env<R>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> R) -> R {
        let _guard = lock();
        let _restore = Restore(
            vars.iter()
                .map(|(k, _)| ((*k).to_string(), std::env::var(k).ok()))
                .collect(),
        );
        for (k, v) in vars {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        f()
    }
}

#[cfg(test)]
mod tests {
    use super::test_env::with_env;
    use super::*;

    fn ok(root: &str, candidate: &str) -> PathBuf {
        confine(Path::new(root), Path::new(candidate)).unwrap()
    }

    /// Assert the escape, and hand the error back for inspection.
    fn escapes(root: &str, candidate: &str) -> BoughError {
        let err = confine(Path::new(root), Path::new(candidate))
            .expect_err(&format!("confine({root:?}, {candidate:?}) must reject"));
        assert_eq!(err.status(), 400);
        assert_eq!(err.name(), "PathError");
        err
    }

    // ---- the layout ---------------------------------------------------------

    /// A plugin is a DIRECTORY. A loose file in `plugins/` is not one, and a
    /// dotfile directory is the bookkeeping of whatever put the others there.
    #[test]
    fn plugin_discovery_takes_directories_only_and_sorts_them() {
        let root = std::env::temp_dir().join(format!("bough-plugins-{}", uuid::Uuid::new_v4()));
        for name in ["zeta", "acme", ".git"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        std::fs::write(root.join("README.md"), "").unwrap();

        let names: Vec<String> = plugin_dirs_in(&root)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["acme", "zeta"]);
        assert!(
            plugin_dirs_in(&root.join("absent")).is_empty(),
            "no plugins directory is the normal case, not an error"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bough_home_relocates_the_entire_tree() {
        with_env(
            &[("BOUGH_HOME", Some("/fake/root")), ("BOUGH_DB", None)],
            || {
                assert_eq!(bough_home(), PathBuf::from("/fake/root"));
                assert_eq!(bough_path(&["x", "y"]), PathBuf::from("/fake/root/x/y"));
                assert_eq!(db_path(), PathBuf::from("/fake/root/bough.db"));
                assert_eq!(artifacts_dir(), PathBuf::from("/fake/root/artifacts"));
                assert_eq!(
                    artifacts_dir_for("s1"),
                    PathBuf::from("/fake/root/artifacts/s1")
                );
                assert_eq!(comments_dir(), PathBuf::from("/fake/root/comments"));
                assert_eq!(
                    comments_path_for("s1"),
                    PathBuf::from("/fake/root/comments/s1.json")
                );
                assert_eq!(attachments_dir(), PathBuf::from("/fake/root/attachments"));
                assert_eq!(scratch_root(), PathBuf::from("/fake/root/scratch"));
                assert_eq!(
                    scratch_dir_for("s1"),
                    PathBuf::from("/fake/root/scratch/s1")
                );
                assert_eq!(workflows_dir(), PathBuf::from("/fake/root/workflows"));
                assert_eq!(
                    workflow_script_path("w7"),
                    PathBuf::from("/fake/root/workflows/w7.js")
                );
                assert_eq!(user_skills_dir(), PathBuf::from("/fake/root/skills"));
                assert_eq!(maps_dir(), PathBuf::from("/fake/root/maps"));
                assert_eq!(
                    map_dir_for("payments-rewrite").unwrap(),
                    PathBuf::from("/fake/root/maps/payments-rewrite")
                );
                assert_eq!(theme_path(), PathBuf::from("/fake/root/theme.json"));
                assert_eq!(
                    model_settings_path(),
                    PathBuf::from("/fake/root/model.json")
                );
                assert_eq!(env_path(), PathBuf::from("/fake/root/env"));
                assert_eq!(mcp_registry_path(), PathBuf::from("/fake/root/mcp.json"));
                assert_eq!(mcp_auth_path(), PathBuf::from("/fake/root/mcp-auth.json"));
                assert_eq!(logs_dir(), PathBuf::from("/fake/root/logs"));
            },
        );
    }

    #[test]
    fn unset_or_blank_bough_home_falls_back_to_dot_bough() {
        // A blank override is a shell accident (`BOUGH_HOME= bough`), not a
        // request to put the data root at the filesystem root or the cwd.
        for v in [None, Some(""), Some("   ")] {
            with_env(&[("BOUGH_HOME", v)], || {
                let home = bough_home();
                assert!(home.ends_with(".bough"), "{home:?}");
                assert!(home.is_absolute(), "{home:?}");
            });
        }
    }

    #[test]
    fn env_is_read_per_call_not_cached() {
        with_env(&[("BOUGH_HOME", Some("/first/root"))], || {
            assert_eq!(bough_home(), PathBuf::from("/first/root"));
            std::env::set_var("BOUGH_HOME", "/second/root");
            assert_eq!(bough_home(), PathBuf::from("/second/root"));
            assert_eq!(db_path(), PathBuf::from("/second/root/bough.db"));
        });
    }

    #[test]
    fn bough_db_overrides_the_database_path_outright() {
        with_env(
            &[
                ("BOUGH_HOME", Some("/fake/root")),
                ("BOUGH_DB", Some(":memory:")),
            ],
            || {
                assert_eq!(db_path(), PathBuf::from(":memory:"));
            },
        );
    }

    #[test]
    fn comment_sidecars_live_outside_the_artifacts_tree() {
        // A sidecar under artifacts/ would be walked by every listing and
        // served as an artifact itself.
        with_env(&[("BOUGH_HOME", Some("/fake/root"))], || {
            let artifacts = artifacts_dir();
            assert!(!comments_dir().starts_with(&artifacts));
            assert!(!comments_path_for("s1").starts_with(&artifacts));
            assert!(confine(&artifacts, &comments_path_for("s1")).is_err());
        });
    }

    #[test]
    fn an_effort_name_cannot_steer_a_map_out_of_the_maps_tree() {
        // The name reaches map_dir_for from a model-authored string, so
        // `../theme.json` is the case worth stopping.
        with_env(&[("BOUGH_HOME", Some("/fake/root"))], || {
            assert!(map_dir_for("../theme.json").is_err());
            assert!(map_dir_for("a/../..").is_err());
            assert!(map_dir_for("/etc").is_err());
        });
    }

    // ---- confine: the accepting direction -----------------------------------

    #[test]
    fn confine_returns_an_absolute_path_under_the_root() {
        assert_eq!(ok("/a/b", "c"), PathBuf::from("/a/b/c"));
        assert_eq!(ok("/a/b", "c/d/e.html"), PathBuf::from("/a/b/c/d/e.html"));
        assert_eq!(ok("/a/b", "./c"), PathBuf::from("/a/b/c"));
    }

    #[test]
    fn confine_accepts_dotdot_that_lands_back_inside() {
        // The check is on the RESOLVED path, not on the presence of a ".."
        // segment — rejecting the substring would break legitimate callers.
        assert_eq!(ok("/a/b", "c/../d"), PathBuf::from("/a/b/d"));
        assert_eq!(ok("/a/b", "c/d/../../e"), PathBuf::from("/a/b/e"));
    }

    #[test]
    fn confine_accepts_an_absolute_candidate_already_inside() {
        assert_eq!(ok("/a/b", "/a/b/c"), PathBuf::from("/a/b/c"));
        assert_eq!(ok("/a/b", "/a/b"), PathBuf::from("/a/b"));
    }

    #[test]
    fn confine_normalizes_the_root_and_empty_candidate_is_the_root() {
        assert_eq!(ok("/a/b/", "c"), PathBuf::from("/a/b/c"));
        assert_eq!(ok("/a/b//", "c"), PathBuf::from("/a/b/c"));
        assert_eq!(ok("/a/./b", "c"), PathBuf::from("/a/b/c"));
        assert_eq!(ok("/a/b", ""), PathBuf::from("/a/b"));
        assert_eq!(ok("/a/b", "."), PathBuf::from("/a/b"));
    }

    #[test]
    fn confine_resolves_a_relative_root_against_the_cwd() {
        let expected = lexical_normalize(&std::env::current_dir().unwrap().join("store").join("x"));
        assert_eq!(ok("store", "x"), expected);
    }

    #[test]
    fn confine_handles_the_filesystem_root_without_doubling() {
        assert_eq!(ok("/", "etc"), PathBuf::from("/etc"));
        assert_eq!(ok("/", "/etc"), PathBuf::from("/etc"));
        // "/.." is "/" — inside, not an escape.
        assert_eq!(ok("/", ".."), PathBuf::from("/"));
    }

    // ---- confine: ".." traversal --------------------------------------------

    #[test]
    fn confine_rejects_dotdot_traversal_out_of_the_root() {
        escapes("/a/b", "..");
        escapes("/a/b", "../c");
        escapes("/a/b", "../../etc/passwd");
        escapes("/a/b", "c/../../d");
        // Landing exactly on the parent of the root is still outside it.
        escapes("/a/b", "../");
    }

    #[test]
    fn confine_rejects_a_chain_whose_segments_each_look_harmless() {
        // Every segment here is a plain name; only the resolved path escapes.
        escapes("/a/b", "x/y/z/../../../../etc/passwd");
    }

    #[test]
    fn confine_error_names_candidate_landing_and_root() {
        // Error text is a product surface: the message must say what failed,
        // the state that caused it, and the move that resolves it.
        let err = escapes("/a/b", "../../etc/passwd");
        let msg = err.to_string();
        assert!(msg.contains("../../etc/passwd"), "{msg}");
        assert!(msg.contains("/etc/passwd"), "{msg}");
        assert!(msg.contains("/a/b"), "{msg}");
        assert!(msg.contains("Use a path that stays under /a/b"), "{msg}");
        assert_eq!(err.status(), 400);
    }

    // ---- confine: absolute escapes ------------------------------------------

    #[test]
    fn confine_rejects_an_absolute_candidate_outside_the_root() {
        escapes("/a/b", "/etc/passwd");
        escapes("/a/b", "/");
        escapes("/a/b", "/a");
    }

    #[test]
    fn confine_rejects_a_string_prefix_sibling() {
        // "/a/bc" starts with "/a/b" as a STRING but is not under it as a PATH.
        escapes("/a/b", "/a/bc");
        escapes("/a/b", "/a/bc/d");
        escapes("/a/b", "../bc/d");
    }

    #[test]
    fn confine_rejects_a_nul_byte() {
        // A NUL truncates at the syscall boundary; reject before the OS sees it.
        escapes("/a/b", "ok\0/../../etc/passwd");
        escapes("/a/b\0", "c");
        let msg = escapes("/a/b", "ok\0/../../etc/passwd").to_string();
        assert!(msg.contains("NUL byte"), "{msg}");
    }

    #[test]
    fn a_session_id_with_traversal_cannot_steer_the_artifact_directory() {
        // The shape of the real caller: a session id arriving in a URL.
        with_env(&[("BOUGH_HOME", Some("/fake/root"))], || {
            assert_eq!(
                confine(&artifacts_dir(), &artifacts_dir_for("s1")).unwrap(),
                PathBuf::from("/fake/root/artifacts/s1")
            );
            assert!(confine(&artifacts_dir(), &artifacts_dir_for("../../etc")).is_err());
            assert!(confine(&artifacts_dir_for("s1"), Path::new("../s2/secret.html")).is_err());
        });
    }

    // ---- confine: symlink-shaped inputs -------------------------------------

    #[test]
    fn confine_is_lexical_around_a_real_symlink() {
        let tmp = std::env::temp_dir().join(format!("bough-paths-test-{}", std::process::id()));
        let root = tmp.join("root");
        let outside = tmp.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "no").unwrap();
        let link = root.join("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        // Lexical resolution collapses "link/.." to the root, so a traversal
        // that routes through the link still resolves outward and is rejected.
        assert!(confine(&root, Path::new("link/../../outside/secret.txt")).is_err());
        assert!(confine(&root, Path::new("link/../..")).is_err());

        // Documented boundary: the link itself resolves inside the root and is
        // ACCEPTED, because confine is lexical and never follows symlinks.
        // Pinned so a later move to fs-based resolution is deliberate.
        assert_eq!(
            confine(&root, Path::new("link")).unwrap(),
            root.join("link")
        );
        assert_eq!(
            confine(&root, Path::new("link/secret.txt")).unwrap(),
            root.join("link/secret.txt")
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn symlinked_root_and_its_realpath_are_different_namespaces() {
        // The macOS /tmp -> /private/tmp shape: a candidate that has been
        // through realpath no longer matches a root that has not, so callers
        // must build both from the same source — bough_path().
        escapes("/tmp/store", "/private/tmp/store/a.html");
        assert_eq!(
            ok("/private/tmp/store", "/private/tmp/store/a.html"),
            PathBuf::from("/private/tmp/store/a.html")
        );
    }
}
