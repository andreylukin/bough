//! Where hooks come from: the ones bough ships, the ones you cloned, and the
//! ones you wrote.
//!
//! THREE ROOTS, ONE ORDER — bundled, then git sources in the order you added
//! them, then your own `~/.bough/hooks`. Later wins a name collision, so a
//! file you wrote always beats a repo's file of the same name, and a repo's
//! always beats a bundled one. The order is the answer to "which one is
//! running", and it is the same order the panel prints.
//!
//! ## Ids are source-qualified
//!
//! Two repos WILL both ship a `guard.lua`. So a hook's identity — the thing
//! the off-switch, the panel row and the activity map all key on — is
//! `<source>/<file>`, never the bare file name.
//!
//! ## On by default is not the same answer everywhere
//!
//! A file you dropped in `~/.bough/hooks` is ON: that is the whole
//! installation story, and a file that sat inert until you found a panel
//! would be a bug report. A BUNDLED or CLONED hook is OFF until you say
//! otherwise, because neither arrived by you writing it — one came with an
//! upgrade, the other came from a stranger's repository, and both run
//! in-process as you on the next turn. Opting in is one keystroke; opting out
//! of something already running is a support question.
//!
//! ## Nothing here fetches on its own
//!
//! `bough hooks update` re-fetches, prints the SHA it moved to, and is the
//! only thing that ever changes cloned bytes. A harness that quietly pulled
//! new code from a repo between turns would be a supply chain with no gate on
//! it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::{bough_path, hooks_dir};

/// Where a source's hooks came from, which decides whether they default on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    /// Shipped inside the binary, materialized on first use.
    Bundled,
    /// Cloned from a git repository named in `hooks.json`.
    Git,
    /// The `hooks/` directory of a plugin in `~/.bough/plugins/<name>`.
    Plugin,
    /// `~/.bough/hooks` — the files you wrote.
    Local,
}

impl SourceKind {
    /// Is a hook from this source on when nothing has been said about it?
    ///
    /// A PLUGIN IS OFF, for the same reason a clone is: a plugin directory is
    /// the unit you get from someone else, and the whole argument below is
    /// that code which arrived rather than being written must be turned on
    /// deliberately. One keystroke turns it on; nothing turns back time on a
    /// hook that already ran.
    pub fn on_by_default(self) -> bool {
        matches!(self, SourceKind::Local)
    }
}

/// The bundled adapters, and the harness each one adapts.
///
/// THEY ARE THEIR OWN SOURCES, not two files in the bundle. Everything bough
/// adopts from another harness — that harness's skills, its installed plugins'
/// skills, and the adapter that reads its settings and hooks — belongs under
/// one name you can switch, because "stop taking anything from Claude Code" is
/// one decision and was previously three places. Their group is the harness's
/// name, which is what `skills::foreign` labels its rungs with, so the section
/// holds all of it.
pub const ADAPTERS: [(&str, &str); 2] = [
    (crate::skills::foreign::CLAUDE_CODE, "claude-code.lua"),
    (crate::skills::foreign::CODEX, "codex.lua"),
];

/// Bundled hooks that are ON without being asked for, by id.
///
/// THE EXCEPTION TO "BUNDLED IS OFF", AND THE ARGUMENT FOR IT. The rule above
/// exists so an upgrade never starts running code you did not ask for. These
/// two do not run code of their own: they ADAPT a configuration you already
/// have, and they are inert on a machine that has none — no `.claude` or
/// `.codex` directory means every read returns nil and every dispatch folds to
/// nothing. The failure they prevent is the one that actually happens: a user
/// opens a repo whose guardrails live in `.claude/settings.json`, bough
/// ignores them, and nothing anywhere says so.
///
/// What they can do once a config IS present is exactly what that config says
/// — including running its commands. That is the same trust the user already
/// extended to the other harness by writing the file, and it is revocable in
/// one keystroke (`^x`), which the state file records explicitly so this list
/// changing its mind later cannot re-enable something you turned off.
pub const DEFAULT_ON: [&str; 2] = ["claude-code/claude-code.lua", "codex/codex.lua"];

/// One place hooks are discovered from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookSource {
    /// The id prefix: `local`, `bundled`, or a slug of the repo.
    pub name: String,
    pub kind: SourceKind,
    pub dir: PathBuf,
    /// Git sources only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The ref you asked for — a branch, a tag, a commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    /// The commit actually checked out. This is the thing to compare when
    /// asking "did what I am running change?"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    /// Only these file names, when a directory is shared by more than one
    /// source. `None` is "everything in the directory", which is every source
    /// but the harness adapters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
}

/// `~/.bough/hooks.json` — the git sources you added.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SourcesFile {
    #[serde(default)]
    pub sources: Vec<GitSource>,
}

/// One entry in `hooks.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSource {
    pub repo: String,
    /// What to check out. Defaults to the remote's default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    /// Subdirectory holding the `.lua` files, when they are not at the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// The commit last checked out, recorded so `update` can say what moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
}

impl GitSource {
    /// The id prefix and clone directory name for this repo: `owner-name`,
    /// slugified. Stable across `rev` changes, because the identity is the
    /// REPOSITORY — pinning to a different tag must not orphan every off
    /// switch you set.
    pub fn slug(&self) -> String {
        let trimmed = self
            .repo
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .to_lowercase();
        let parts: Vec<&str> = trimmed
            .rsplit(['/', ':'])
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let joined = parts.join("-");
        joined
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim_matches('-')
            .to_string()
    }
}

pub fn sources_path() -> PathBuf {
    bough_path(&["hooks.json"])
}

/// Where cloned repositories live. Outside `hooks/` so a clone never lands in
/// the directory whose contents you own.
pub fn repos_dir() -> PathBuf {
    bough_path(&["hook-repos"])
}

pub fn bundled_hooks_dir() -> PathBuf {
    bough_path(&["bundled-hooks", env!("CARGO_PKG_VERSION")])
}

/// The hook files shipped with bough. Materialized to disk like the bundled
/// skills, and for the same reason: a hook may sit beside data files it reads,
/// and a path is the only thing a `bough.fs.read` can take.
static BUNDLED: include_dir::Dir<'_> = include_dir::include_dir!("$CARGO_MANIFEST_DIR/hooks");

/// Write the bundled hooks to disk. Overwrites in place — the embedded bytes
/// are the source of truth, and a user edit to a bundled file is not a thing
/// this store tries to preserve (copy it into `~/.bough/hooks` to own it).
pub fn materialize_bundled(dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for file in BUNDLED.files() {
        std::fs::write(dest.join(file.path()), file.contents())?;
    }
    Ok(())
}

fn ensure_bundled() -> Option<PathBuf> {
    let dest = bundled_hooks_dir();
    // Best effort: a bundle that cannot be written is a source with nothing
    // in it, which discovery already handles.
    materialize_bundled(&dest).ok()?;
    Some(dest)
}

pub fn read_sources_file(path: &Path) -> SourcesFile {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn write_sources_file(path: &Path, file: &SourcesFile) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(file).unwrap_or_default())
}

/// Every source, in load order: bundled, then each git source, then each
/// plugin, then local.
pub fn all_sources() -> Vec<HookSource> {
    sources_from(
        &sources_path(),
        &repos_dir(),
        &crate::paths::plugins_dir(),
        &hooks_dir(),
        &crate::plugins::state(),
    )
}

/// The injectable form — tests point all four somewhere temporary and pass the
/// switchboard rather than moving `BOUGH_HOME` to reach the real one.
pub fn sources_from(
    sources_at: &Path,
    repos: &Path,
    plugins: &Path,
    local: &Path,
    switches: &crate::plugins::PluginState,
) -> Vec<HookSource> {
    // A GROUP THAT IS OFF IS NOT A SOURCE, rather than a source whose hooks all
    // happen to be off. Its files stop being loaded, so the listeners they
    // registered stop existing — which is the only way to un-register one — and
    // the per-hook switches under it are left exactly as they were for when you
    // turn it back on.
    //
    // THIS APPLIES TO EVERY GROUP, not only plugins. It used to be plugins
    // alone, which made the switch on `bundled` or on `local` cosmetic: the
    // panel printed "its source is off" while the hook stayed loaded and went
    // on firing. A switch that reports a state it does not enforce is worse
    // than no switch.
    let on = |name: &str| switches.plugin_on(name);
    let mut out = Vec::new();
    let bundle = ensure_bundled();
    // The adapters first and each under its harness's name, so they keep the
    // load position they had inside the bundle and a harness's switch reaches
    // the thing that reads that harness's configuration.
    for (harness, file) in ADAPTERS {
        let Some(dir) = bundle.clone().filter(|_| on(harness)) else {
            continue;
        };
        out.push(HookSource {
            name: harness.into(),
            kind: SourceKind::Bundled,
            dir,
            repo: None,
            rev: None,
            sha: None,
            files: Some(vec![file.to_string()]),
        });
    }
    if let Some(dir) = bundle.filter(|_| on("bundled")) {
        out.push(HookSource {
            name: "bundled".into(),
            kind: SourceKind::Bundled,
            dir,
            repo: None,
            rev: None,
            sha: None,
            // Everything the bundle ships EXCEPT the adapters, which are
            // sources of their own directly above.
            files: None,
        });
    }
    for git in read_sources_file(sources_at).sources {
        let slug = git.slug();
        if !on(&slug) {
            continue;
        }
        out.push(HookSource {
            name: slug.clone(),
            kind: SourceKind::Git,
            dir: match &git.dir {
                Some(sub) => repos.join(&slug).join(sub),
                None => repos.join(&slug),
            },
            repo: Some(git.repo),
            rev: git.rev,
            sha: git.sha,
            files: None,
        });
    }
    // A plugin's `hooks/` directory, named by the plugin, so a hook inside it
    // is `<plugin>/<file>.lua` — the same source-qualified id shape a repo's
    // hooks get, and for the same reason: two plugins WILL both ship a
    // `guard.lua`.
    for plugin in crate::paths::plugin_dirs_in(plugins) {
        let dir = plugin.join("hooks");
        if !dir.is_dir() {
            continue;
        }
        let Some(name) = plugin.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !on(name) {
            continue;
        }
        out.push(HookSource {
            name: name.to_string(),
            kind: SourceKind::Plugin,
            dir,
            repo: None,
            rev: None,
            sha: None,
            files: None,
        });
    }
    if on("local") {
        out.push(HookSource {
            name: "local".into(),
            kind: SourceKind::Local,
            dir: local.to_path_buf(),
            repo: None,
            rev: None,
            sha: None,
            files: None,
        });
    }
    out
}

/// The `.lua` files a source contributes, name-sorted, as `(id, path)`.
///
/// A source with `files` takes exactly those and a source without takes
/// everything EXCEPT what another source has claimed — otherwise the bundle's
/// adapters would be listed twice, once under the harness they adapt and once
/// under `bundled`.
pub fn files_in(source: &HookSource) -> Vec<(String, PathBuf)> {
    let claimed: Vec<&str> = ADAPTERS.iter().map(|(_, f)| *f).collect();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&source.dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "lua") && p.is_file())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
            match (&source.files, name) {
                (Some(only), Some(name)) => only.contains(&name),
                (Some(_), None) => false,
                // Only the BUNDLE's copies are claimed; a repo or a plugin that
                // happens to ship a `codex.lua` keeps it.
                (None, Some(name)) => {
                    source.kind != SourceKind::Bundled || !claimed.contains(&name.as_str())
                }
                (None, None) => false,
            }
        })
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (format!("{}/{name}", source.name), path)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repo_url_slugs_to_owner_name_however_it_was_written() {
        let slug = |repo: &str| {
            GitSource {
                repo: repo.into(),
                rev: None,
                dir: None,
                sha: None,
            }
            .slug()
        };
        assert_eq!(
            slug("https://github.com/someone/rust-hooks"),
            "someone-rust-hooks"
        );
        assert_eq!(
            slug("https://github.com/someone/rust-hooks.git"),
            "someone-rust-hooks"
        );
        assert_eq!(
            slug("git@github.com:someone/rust-hooks.git"),
            "someone-rust-hooks"
        );
        assert_eq!(
            slug("https://github.com/someone/rust-hooks/"),
            "someone-rust-hooks"
        );
        // The slug is the REPO's identity, so re-pinning to another tag keeps
        // every off switch pointed at the same hooks.
        assert_eq!(
            GitSource {
                repo: "https://github.com/someone/rust-hooks".into(),
                rev: Some("v9".into()),
                dir: None,
                sha: None,
            }
            .slug(),
            slug("https://github.com/someone/rust-hooks")
        );
    }

    #[test]
    fn only_local_hooks_are_on_when_nothing_has_been_said_about_them() {
        assert!(SourceKind::Local.on_by_default());
        assert!(
            !SourceKind::Git.on_by_default(),
            "a stranger's repo does not start running because you cloned it"
        );
        assert!(
            !SourceKind::Bundled.on_by_default(),
            "an upgrade must not start running code you never turned on"
        );
    }

    #[test]
    fn ids_are_source_qualified_so_two_repos_can_ship_the_same_file_name() {
        let dir = std::env::temp_dir().join(format!("bough-src-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("guard.lua"), "").unwrap();
        std::fs::write(dir.join("other.lua"), "").unwrap();
        std::fs::write(dir.join("notes.md"), "").unwrap();
        let source = HookSource {
            name: "someone-rust-hooks".into(),
            kind: SourceKind::Git,
            dir: dir.clone(),
            repo: None,
            rev: None,
            sha: None,
            files: None,
        };
        let ids: Vec<String> = files_in(&source).into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            ids,
            [
                "someone-rust-hooks/guard.lua",
                "someone-rust-hooks/other.lua"
            ],
            "name-sorted, .lua only, prefixed by the source"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_load_order_is_bundled_then_each_repo_then_your_own() {
        let root = std::env::temp_dir().join(format!("bough-order-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("hooks")).unwrap();
        let sources_at = root.join("hooks.json");
        write_sources_file(
            &sources_at,
            &SourcesFile {
                sources: vec![
                    GitSource {
                        repo: "https://github.com/a/one".into(),
                        rev: None,
                        dir: None,
                        sha: None,
                    },
                    GitSource {
                        repo: "https://github.com/b/two".into(),
                        rev: None,
                        dir: Some("hooks".into()),
                        sha: None,
                    },
                ],
            },
        )
        .unwrap();
        // One plugin with hooks and one without: only the first is a source.
        std::fs::create_dir_all(root.join("plugins/acme/hooks")).unwrap();
        std::fs::create_dir_all(root.join("plugins/skills-only/skills")).unwrap();
        let sources = sources_from(
            &sources_at,
            &root.join("repos"),
            &root.join("plugins"),
            &root.join("hooks"),
            &crate::plugins::PluginState::all_on(),
        );
        let names: Vec<&str> = sources.iter().map(|s| s.name.as_str()).collect();
        // Bundled may be absent when the bundle cannot be written; the ORDER
        // of what is present is what this pins.
        // The adapters lead, each under the harness it adapts, keeping the
        // load position they had when they were two files in the bundle.
        let expected: Vec<&str> = [
            "claude-code",
            "codex",
            "bundled",
            "a-one",
            "b-two",
            "acme",
            "local",
        ]
        .into_iter()
        .filter(|n| names.contains(n))
        .collect();
        assert_eq!(names, expected);
        assert!(sources.last().unwrap().kind == SourceKind::Local);
        // A `dir` lands under the clone, not beside it.
        let two = sources.iter().find(|s| s.name == "b-two").unwrap();
        assert!(two.dir.ends_with("b-two/hooks"), "{:?}", two.dir);
        // A plugin contributes its `hooks/` under the PLUGIN's name, so the
        // ids inside it are `acme/<file>.lua`, and it is off until asked for.
        let acme = sources.iter().find(|s| s.name == "acme").unwrap();
        assert!(acme.dir.ends_with("acme/hooks"), "{:?}", acme.dir);
        assert_eq!(acme.kind, SourceKind::Plugin);
        assert!(!acme.kind.on_by_default());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// EVERY group's switch drops the source, not only a plugin's. When this
    /// was plugins-only the switch on `bundled` and on `local` was cosmetic:
    /// the panel said "its source is off" while the hook stayed loaded and
    /// went on firing.
    #[test]
    fn any_group_switched_off_stops_being_a_hook_source() {
        let root = std::env::temp_dir().join(format!("bough-grp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("hooks")).unwrap();
        std::fs::write(root.join("hooks/mine.lua"), "").unwrap();
        let names = |off: &str| -> Vec<String> {
            sources_from(
                &root.join("hooks.json"),
                &root.join("repos"),
                &root.join("plugins"),
                &root.join("hooks"),
                &crate::plugins::PluginState {
                    off: vec![off.into()],
                    ..Default::default()
                },
            )
            .into_iter()
            .map(|s| s.name)
            .collect()
        };
        assert!(!names("local").contains(&"local".to_string()));
        assert!(names("bundled").contains(&"local".to_string()));
        assert!(!names("bundled").contains(&"bundled".to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_plugin_switched_off_is_not_a_hook_source() {
        let root = std::env::temp_dir().join(format!("bough-off-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("hooks")).unwrap();
        std::fs::create_dir_all(root.join("plugins/acme/hooks")).unwrap();
        std::fs::create_dir_all(root.join("plugins/other/hooks")).unwrap();
        let off = crate::plugins::PluginState {
            off: vec!["acme".into()],
            ..Default::default()
        };
        let names: Vec<String> = sources_from(
            &root.join("hooks.json"),
            &root.join("repos"),
            &root.join("plugins"),
            &root.join("hooks"),
            &off,
        )
        .into_iter()
        .map(|s| s.name)
        .collect();
        assert!(!names.contains(&"acme".to_string()), "{names:?}");
        assert!(names.contains(&"other".to_string()), "{names:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
