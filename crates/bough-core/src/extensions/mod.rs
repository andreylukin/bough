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
/// Plugins go FIRST, because later wins here: a plugin arrived from somewhere
/// else, so the loose file you wrote by hand must be able to shadow one of its
/// names, and the project's file must be able to shadow both.
pub fn extension_dirs(workspace: &Path) -> Vec<PathBuf> {
    extension_sources(workspace)
        .into_iter()
        .map(|(_, d)| d)
        .collect()
}

/// [`extension_dirs`], each directory paired with the SWITCH GROUP that owns
/// it: a bough plugin's name, else the rung's own — `local` for the ones you
/// wrote, `project` for the ones this checkout ships.
///
/// EVERY RUNG IS A GROUP NOW. It used to be plugins only, and a loose file in
/// `~/.bough/extensions` had no switch at all: the answer to "stop binding this
/// tool" was to move the file. The group is the id prefix, and it is the same
/// prefix the skills in that tier get, because "turn off what I wrote" is one
/// question and not two.
pub fn extension_sources(workspace: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = crate::paths::plugin_dirs()
        .into_iter()
        .filter_map(|p| {
            let name = p.file_name().and_then(|n| n.to_str())?.to_string();
            Some((name, p.join("extensions")))
        })
        .collect();
    out.push(("local".to_string(), bough_path(&["extensions"])));
    out.push((
        "project".to_string(),
        workspace.join(".agents").join("extensions"),
    ));
    out
}

/// The loadable files in one directory as `(name, path)`: `*.js` / `*.mjs` /
/// `*.cjs` / `*.ts` at the top level, plus `<sub>/index.*` one level down so an
/// extension with helper modules is a directory. Sorted by NAME, so binding
/// order is stable across runs rather than whatever order the filesystem
/// answered in.
///
/// The name is the path relative to `dir` — `sub/index.js`, not `index.js` —
/// because it is what a plugin's switch is keyed on, and two directory
/// extensions in one plugin would otherwise share one switch.
pub fn files_in(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if let Some(index) = crate::plugins::extension_index(&path) {
                let leaf = index
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                out.push((format!("{name}/{leaf}"), index));
            }
            continue;
        }
        if crate::plugins::is_loadable(&path) {
            out.push((name, path));
        }
    }
    out.sort();
    out
}

/// Every extension file for a workspace, in binding order, minus the ones a
/// plugin switch turned off.
pub fn discover(workspace: &Path) -> Vec<PathBuf> {
    discover_with(workspace, &crate::plugins::state())
}

/// [`discover`] against a given switchboard.
///
/// A SWITCHED-OFF EXTENSION IS NOT DISCOVERED, rather than discovered and then
/// skipped at binding: the file list is what the cache fingerprints and what
/// the probe loads, so dropping it here is what makes the prompt section and
/// the worker's parameter list agree with the switch. They are the same list —
/// the module header's load-bearing claim — and this is upstream of both.
pub fn discover_with(workspace: &Path, switches: &crate::plugins::PluginState) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for (group, dir) in extension_sources(workspace) {
        if !switches.plugin_on(&group) {
            continue;
        }
        out.extend(files_in(&dir).into_iter().filter_map(|(rel, path)| {
            switches
                .item_on(&group, &crate::plugins::extension_id(&group, &rel))
                .then_some(path)
        }));
    }
    out
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

    /// A plugin bundles its extensions with its hooks and skills, and binds
    /// EARLIEST — later wins here, so your own loose file can still shadow a
    /// name a plugin took.
    #[test]
    fn a_plugin_directory_contributes_extensions_before_your_own_and_the_projects() {
        let home = tmp_ws("plugin-home");
        std::fs::create_dir_all(home.join("plugins").join("acme").join("extensions")).unwrap();
        std::fs::write(
            home.join("plugins")
                .join("acme")
                .join("extensions")
                .join("gh.js"),
            "",
        )
        .unwrap();
        let ws = tmp_ws("plugin-ws");

        let dirs = crate::paths::test_env::with_env(&[("BOUGH_HOME", home.to_str())], || {
            extension_dirs(&ws)
        });
        assert_eq!(
            dirs,
            vec![
                home.join("plugins").join("acme").join("extensions"),
                home.join("extensions"),
                ws.join(".agents").join("extensions"),
            ]
        );
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// A plugin's extension has a switch; the loose files you wrote do not,
    /// and must not be reachable by one.
    #[test]
    fn a_switched_off_plugin_extension_is_not_discovered() {
        let home = tmp_ws("switch-home");
        let ext = home.join("plugins").join("acme").join("extensions");
        std::fs::create_dir_all(ext.join("big")).unwrap();
        std::fs::write(ext.join("gh.js"), "").unwrap();
        std::fs::write(ext.join("big").join("index.js"), "").unwrap();
        let ws = tmp_ws("switch-ws");

        let names = |state: &crate::plugins::PluginState| -> Vec<String> {
            crate::paths::test_env::with_env(&[("BOUGH_HOME", home.to_str())], || {
                discover_with(&ws, state)
            })
            .into_iter()
            .map(|p| p.strip_prefix(&ext).unwrap().to_string_lossy().into_owned())
            .collect()
        };

        assert_eq!(
            names(&crate::plugins::PluginState::all_on()),
            vec!["big/index.js", "gh.js"],
            "on until said otherwise — a switch does not change the default"
        );
        assert_eq!(
            names(&crate::plugins::PluginState {
                off: vec!["acme/extensions/gh.js".into()],
                ..Default::default()
            }),
            vec!["big/index.js"],
            "one file, not the whole plugin"
        );
        assert!(
            names(&crate::plugins::PluginState {
                off: vec!["acme".into()],
                ..Default::default()
            })
            .is_empty(),
            "the plugin's switch takes the lot"
        );
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&ws);
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
