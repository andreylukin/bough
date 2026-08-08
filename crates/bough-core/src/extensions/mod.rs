//! Extensions: JavaScript a user drops on disk, bound into every program's
//! scope alongside the eighteen host functions.
//!
//! THE SHAPE, AND WHY IT IS THIS ONE. An extension is not a tool and not a
//! command. `turn/runner.rs` states the rule this module obeys: a per-session
//! entry in the LLM's tool list would split the provider's prompt cache, so
//! "capabilities are granted through host functions inside `run_steps` and the
//! prompt sections that document them, never by adding a tool." An extension
//! is exactly that — one more name in the program's scope, documented in one
//! more prompt section. The model's tool list never changes.
//!
//! ## The functions never cross the wire
//!
//! A bridged host function is a Rust closure the worker calls over stdin. An
//! extension function is NOT: the sidecar `require()`s the file and binds the
//! exports directly into the program's scope. Nothing about it reaches Rust,
//! which is why [`crate::harness::protocol::HOST_FN_NAMES`] stays the closed
//! eighteen and `HostFns::get` keeps its exhaustive no-default match. The
//! drift pin is untouched because there is nothing here to drift against.
//!
//! The consequence, and it is deliberate: an extension has no handle to the
//! session — no db, no recorder, no artifacts, no `ask`. What it has is the
//! bridged host functions themselves, which are in scope for it exactly as
//! they are for the program, so an extension composes `bash()` rather than
//! reimplementing it (and a shell run that way still lands in the tag
//! history).
//!
//! ## The file is the source of truth for BOTH consumers
//!
//! The eighteen have two hand-synced lists — `BASE_HOST_FNS` for the prompt
//! grant and the `HostFns` struct for the binding — and `boot.rs` shouts
//! about it ("BOTH HALVES ARE REQUIRED AND NEITHER IS SUFFICIENT"). This
//! surface does not reproduce that. One probe reads the exports out of the
//! file, and the resulting [`ExtensionFn`] list feeds the prompt section and
//! the worker binding alike. A function that is documented is bound, because
//! they are the same list.
//!
//! ## Not a security boundary
//!
//! An extension is arbitrary JavaScript running with the server's full
//! authority, exactly like the programs it sits beside (spec §2.2). Nothing
//! here sandboxes it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::harness::protocol::ExtensionFn;
use crate::paths::bough_path;

/// Everything one workspace's extensions contribute to a turn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extensions {
    /// The files, in binding order — the sidecar `require()`s these.
    pub files: Vec<PathBuf>,
    /// Every exported function, in binding order. Feeds the prompt section
    /// and the worker's parameter list from one list, on purpose.
    pub fns: Vec<ExtensionFn>,
    /// Files that would not load, and exports that were refused (a reserved
    /// name, a non-identifier). Surfaced rather than swallowed: an extension
    /// that silently is not there is a support question.
    pub errors: Vec<String>,
}

impl Extensions {
    pub fn is_empty(&self) -> bool {
        self.fns.is_empty() && self.errors.is_empty()
    }
}

/// Where extensions come from, in binding order: the ones you wrote for every
/// project, then the ones this project ships. Later wins a name collision,
/// mirroring `hooks::sources` — a project's file beats your global one,
/// because the project is the more specific answer to "which one is running".
pub fn extension_dirs(workspace: &Path) -> Vec<PathBuf> {
    vec![
        bough_path(&["extensions"]),
        workspace.join(".agents").join("extensions"),
    ]
}

/// The loadable files in one directory: `*.js` / `*.mjs` / `*.cjs` / `*.ts`
/// at the top level, plus `<sub>/index.*` one level down so an extension with
/// helper modules is a directory. Sorted, so binding order is stable across
/// runs rather than whatever order the filesystem answered in.
fn files_in(dir: &Path) -> Vec<PathBuf> {
    const EXTS: [&str; 4] = ["js", "mjs", "cjs", "ts"];
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = Vec::new();
    let mut names: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    names.sort();
    for path in names {
        if path.is_dir() {
            for ext in EXTS {
                let idx = path.join(format!("index.{ext}"));
                if idx.is_file() {
                    out.push(idx);
                    break;
                }
            }
            continue;
        }
        let is_loadable = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| EXTS.contains(&e));
        if is_loadable {
            out.push(path);
        }
    }
    out
}

/// Every extension file for a workspace, in binding order.
pub fn discover(workspace: &Path) -> Vec<PathBuf> {
    extension_dirs(workspace)
        .iter()
        .flat_map(|d| files_in(d))
        .collect()
}

/// What the cache keys on: the files AND their mtime/len, so editing an
/// extension takes effect on the next turn instead of on the next restart.
/// A daily driver whose plugin edits need a server bounce is a bug.
type Fingerprint = Vec<(PathBuf, Option<(std::time::SystemTime, u64)>)>;

fn fingerprint(files: &[PathBuf]) -> Fingerprint {
    files
        .iter()
        .map(|f| {
            let stamp = std::fs::metadata(f)
                .ok()
                .and_then(|m| m.modified().ok().map(|t| (t, m.len())));
            (f.clone(), stamp)
        })
        .collect()
}

#[allow(clippy::type_complexity)]
static CACHE: OnceLock<Mutex<HashMap<PathBuf, (Fingerprint, Arc<Extensions>)>>> = OnceLock::new();

/// The workspace's extensions, probed at most once per edit.
///
/// Synchronous on purpose: the only caller is `prepare_turn`, which is the
/// turn's synchronous head, and the probe is one short-lived sidecar per
/// workspace per edit — not per turn.
pub fn for_workspace(workspace: &Path) -> Arc<Extensions> {
    let files = discover(workspace);
    if files.is_empty() {
        return Arc::new(Extensions::default());
    }
    let print = fingerprint(&files);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let guard = cache.lock().expect("extension cache");
        if let Some((cached, ext)) = guard.get(workspace) {
            if *cached == print {
                return ext.clone();
            }
        }
    }
    let ext = Arc::new(probe(&files));
    let mut guard = cache.lock().expect("extension cache");
    guard.insert(workspace.to_path_buf(), (print, ext.clone()));
    ext
}

/// Load the files in a throwaway sidecar and read back what they export.
///
/// The probe runs the same worker the programs run, so "what does this file
/// export" is answered by the engine that will bind it, never by parsing
/// JavaScript in Rust.
fn probe(files: &[PathBuf]) -> Extensions {
    match crate::harness::vm::probe_extensions(files) {
        Ok((fns, errors)) => Extensions {
            files: files.to_vec(),
            fns,
            errors,
        },
        Err(e) => Extensions {
            files: files.to_vec(),
            fns: Vec::new(),
            // Reported, not swallowed — see `Extensions::errors`.
            errors: vec![format!("extensions could not be loaded: {e}")],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_ws(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bough-ext-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn discovery_is_global_then_project_and_sorted() {
        let ws = tmp_ws("discover");
        let ws = ws.as_path();
        let dir = ws.join(".agents").join("extensions");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("b.js"), "").expect("write");
        std::fs::write(dir.join("a.js"), "").expect("write");
        std::fs::write(dir.join("notes.md"), "").expect("write");
        // A directory extension is its index file, not the directory.
        std::fs::create_dir_all(dir.join("sub")).expect("mkdir");
        std::fs::write(dir.join("sub").join("index.js"), "").expect("write");

        // Only the project half — the global dir is the developer's own and
        // must not decide whether this test passes.
        let found: Vec<PathBuf> = discover(ws)
            .into_iter()
            .filter(|p| p.starts_with(ws))
            .collect();
        let names: Vec<String> = found
            .iter()
            .map(|p| {
                if p.file_name().and_then(|n| n.to_str()) == Some("index.js") {
                    "sub/index.js".to_string()
                } else {
                    p.file_name().unwrap().to_string_lossy().into_owned()
                }
            })
            .collect();
        assert_eq!(names, vec!["a.js", "b.js", "sub/index.js"]);
    }

    /// The probe is the load-bearing claim of this module: the prompt
    /// documents what the worker bound, because one list produces both. So it
    /// is asserted against a real file in a real engine, never a fixture.
    #[test]
    fn probe_reads_exports_signatures_and_docs_from_the_engine() {
        if crate::harness::vm::runtime_bin().is_none() {
            return; // no bun/node — the sidecar tests all skip together
        }
        let ws = tmp_ws("probe");
        let dir = ws.join(".agents").join("extensions");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("gh.js"),
            r#"
            function pr(owner, repo = "main") { return `${owner}/${repo}`; }
            pr.doc = "Fetch a pull request";
            module.exports = { pr, bash: () => 1, notAFunction: 3 };
            "#,
        )
        .expect("write");

        let files = discover(&ws);
        let files: Vec<PathBuf> = files.into_iter().filter(|p| p.starts_with(&ws)).collect();
        let (fns, errors) = crate::harness::vm::probe_extensions(&files).expect("probe");

        assert_eq!(fns.len(), 1, "only the one usable export is bound: {fns:?}");
        assert_eq!(fns[0].name, "pr");
        assert_eq!(fns[0].signature, "(owner, repo = \"main\")");
        assert_eq!(fns[0].doc.as_deref(), Some("Fetch a pull request"));
        // Shadowing a host function is refused, and SAID so — a silently
        // missing extension is the support question this avoids.
        assert!(
            errors.iter().any(|e| e.contains("bash")),
            "the refused host-fn shadow is reported: {errors:?}"
        );
    }
}
