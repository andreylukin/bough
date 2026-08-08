//! Where OTHER harnesses keep their skills, and how those directories become
//! bough [`SkillSource`]s.
//!
//! bough's own two sources (bundled, `~/.bough/skills`) are fixed paths this
//! module does not touch. Everything here is discovery: a workspace to walk, a
//! plugin registry to read, a pair of user directories that may not exist.
//!
//! THE INVARIANT THIS HOLDS: **discovery never fails a turn.** Every read here
//! is best-effort and every failure is an empty list. A malformed
//! `installed_plugins.json`, a plugin whose install directory was deleted, a
//! marketplace pointing at a path outside itself — all of them mean "that
//! source contributes nothing", never an error the user sees mid-turn.
//!
//! WHAT IS NOT HERE. Plugin `agents/`, `hooks/hooks.json` and `.mcp.json` are
//! out of scope for this module: MCP servers already have their own importer
//! (`bough sync-mcp`), and hooks are a different runtime. Only `skills/` and
//! `commands/` are read.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{SkillSource, SkillSourceName};

/// Directories walked upward from the workspace before giving up. Matches
/// `prompt::project`'s bound for the same reason: a workspace nested absurdly
/// deep must not turn one turn's discovery into an unbounded walk.
const MAX_DEPTH: usize = 24;

/// The per-directory skill folders each harness uses, in precedence order.
/// Codex first because `.agents/` is the cross-vendor convention and the one
/// Codex documents as the open standard; `.claude/` is the vendor-specific
/// fallback.
const PROJECT_SKILL_DIRS: [&str; 2] = [".agents/skills", ".claude/skills"];

fn absolutize(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn is_git_root(dir: &Path) -> bool {
    std::fs::metadata(dir.join(".git")).is_ok()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// A source only if the directory is really there. Discovery returning a path
/// that does not exist is harmless (`list_skills` skips unreadable dirs), but
/// it would show up as a phantom row in the panel.
fn source_if_dir(source: SkillSourceName, dir: PathBuf) -> Option<SkillSource> {
    dir.is_dir().then_some(SkillSource { source, dir })
}

// ---------------------------------------------------------------------------
// Project tier
// ---------------------------------------------------------------------------

/// The in-workspace skill directories that apply to `workspace`, NEAREST
/// FIRST.
///
/// Nearest first because `list_skills` resolves collisions first-wins: the
/// skill checked into the subpackage you are working in should beat the one at
/// the repo root, which is the opposite of how `AGENTS.md` composes (there,
/// later text wins because the blocks are concatenated rather than chosen
/// between). Same intent, inverted mechanics.
///
/// Walking stops at the git root, and with no git root above it only the
/// workspace directory itself is read — adopting `~/.agents/skills` because a
/// session happened to start in a subdirectory of `$HOME` would be a surprise.
pub fn project_sources(workspace: &Path) -> Vec<SkillSource> {
    let start = absolutize(workspace);
    let mut chain: Vec<PathBuf> = Vec::new();
    let mut dir = start.clone();
    for _ in 0..MAX_DEPTH {
        chain.push(dir.clone());
        if is_git_root(&dir) {
            break;
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => {
                chain.clear();
                chain.push(start.clone());
                break;
            }
        }
    }
    chain
        .iter()
        .flat_map(|d| PROJECT_SKILL_DIRS.iter().map(move |sub| d.join(sub)))
        .filter_map(|d| source_if_dir(SkillSourceName::Project, d))
        .collect()
}

// ---------------------------------------------------------------------------
// Foreign user tier
// ---------------------------------------------------------------------------

/// `~/.claude/skills` and `~/.agents/skills`.
///
/// Takes the home directory rather than reading `dirs::home_dir()` so a test
/// never depends on the machine it runs on. These rank BELOW `~/.bough/skills`
/// — a user who wrote a bough skill and a Claude Code skill of the same name
/// meant the bough one here.
pub fn user_sources(home: &Path) -> Vec<SkillSource> {
    [".claude/skills", ".agents/skills"]
        .iter()
        .filter_map(|sub| source_if_dir(SkillSourceName::Foreign, home.join(sub)))
        .collect()
}

// ---------------------------------------------------------------------------
// Claude Code plugins
// ---------------------------------------------------------------------------

/// The skill directories of every INSTALLED Claude Code plugin.
///
/// INSTALLED, not merely indexed — the same distinction `bough sync-mcp` draws
/// (`sync_mcp.rs`): the marketplace cache holds an entry for every plugin ever
/// browsed, and adopting those would put skills in the prompt the user never
/// chose. The registry read here is the same file, and the project-scope rule
/// is the same one: a `scope: "project"` install counts only when its
/// `projectPath` is the workspace.
///
/// A plugin contributes `skills/` and `commands/` when they exist. A bare
/// `SKILL.md` at the plugin root is a single skill in Claude Code; it is NOT
/// adopted here, because bough's invocation token is the folder name and a
/// marketplace install directory is a version string that changes on every
/// update — the skill would rename itself under the user.
pub fn claude_plugin_sources(claude_home: &Path, workspace: &Path) -> Vec<SkillSource> {
    let registry = claude_home.join("plugins").join("installed_plugins.json");
    let Some(plugins) = read_json(&registry)
        .as_ref()
        .and_then(|r| r.get("plugins"))
        .and_then(|p| p.as_object())
        .cloned()
    else {
        return Vec::new();
    };
    let workspace = absolutize(workspace);

    let mut out = Vec::new();
    for raw in plugins.values() {
        let installs: Vec<Value> = match raw {
            Value::Array(a) => a.clone(),
            other => vec![other.clone()],
        };
        for install in installs {
            let Some(path) = install
                .get("installPath")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            if install.get("scope").and_then(|v| v.as_str()) == Some("project")
                && install
                    .get("projectPath")
                    .and_then(|v| v.as_str())
                    .is_none_or(|p| absolutize(Path::new(p)) != workspace)
            {
                continue;
            }
            let root = PathBuf::from(path);
            out.extend(plugin_skill_dirs(&root));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Codex plugins
// ---------------------------------------------------------------------------

/// The skill directories of the plugins listed in a Codex marketplace.
///
/// Codex has no installed-plugins registry of the Claude Code shape; a
/// marketplace file IS the list, at `~/.agents/plugins/marketplace.json`
/// (personal) or `$REPO_ROOT/.agents/plugins/marketplace.json` (repo-scoped).
/// Each entry's `source.path` is `./`-relative TO THE MARKETPLACE ROOT, and a
/// path that escapes that root is dropped rather than followed — a checked-in
/// marketplace is untrusted input, and `../../..` in a `source.path` would let
/// a repo point bough's skill loader at any directory on the machine.
pub fn codex_marketplace_sources(marketplace: &Path) -> Vec<SkillSource> {
    let Some(root) = marketplace.parent().map(absolutize) else {
        return Vec::new();
    };
    let Some(doc) = read_json(marketplace) else {
        return Vec::new();
    };
    let Some(entries) = doc.get("plugins").and_then(|p| p.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries {
        // `{"source": {"path": "./x"}}` is the documented shape; `{"source":
        // "./x"}` appears in hand-written files and costs one match to accept.
        let path = match entry.get("source") {
            Some(Value::Object(o)) => o.get("path").and_then(|v| v.as_str()),
            Some(Value::String(s)) => Some(s.as_str()),
            _ => None,
        };
        let Some(path) = path.filter(|s| !s.is_empty()) else {
            continue;
        };
        let Ok(plugin_root) = crate::paths::confine(&root, &root.join(path)) else {
            continue;
        };
        out.extend(plugin_skill_dirs(&plugin_root));
    }
    out
}

/// Both harnesses' plugin layouts land on the same two subdirectories, and a
/// manifest may redirect either.
///
/// `skills` and `commands` in the manifest (`.codex-plugin/plugin.json` or
/// `.claude-plugin/plugin.json`) are honored because both scaffolders write
/// them — a plugin that keeps its skills somewhere other than `skills/` is
/// otherwise invisible. They are confined to the plugin root for the same
/// reason the marketplace path is.
///
/// A DECLARED PATH IS EITHER TIER, AND THE MANIFEST DOES NOT SAY WHICH. Both
/// shapes are in the wild: `"skills": "./workflows/"` names a directory
/// holding skill folders, while `"skills": ["./skills/worktrunk"]` names ONE
/// skill folder. bough's unit of discovery is the parent, so a path that
/// holds a `SKILL.md` contributes its parent instead of itself. The cost is
/// that a sibling skill in that parent comes along uninvited; the alternative
/// is that the plugin that spells it the second way contributes nothing.
fn plugin_skill_dirs(root: &Path) -> Vec<SkillSource> {
    let manifest = [".codex-plugin", ".claude-plugin"]
        .iter()
        .find_map(|d| read_json(&root.join(d).join("plugin.json")));

    let mut dirs: Vec<PathBuf> = Vec::new();
    for key in ["skills", "commands"] {
        for rel in manifest
            .as_ref()
            .map(|m| declared_paths(m, key))
            .unwrap_or_default()
        {
            let Ok(path) = crate::paths::confine(root, &root.join(rel)) else {
                continue;
            };
            // The skill folder itself, not a directory of them.
            let path = if path.join("SKILL.md").is_file() {
                match path.parent() {
                    Some(parent) => parent.to_path_buf(),
                    None => continue,
                }
            } else {
                path
            };
            if !dirs.contains(&path) {
                dirs.push(path);
            }
        }
    }
    for conventional in ["skills", "commands"] {
        let path = root.join(conventional);
        if !dirs.contains(&path) {
            dirs.push(path);
        }
    }
    dirs.into_iter()
        .filter_map(|d| source_if_dir(SkillSourceName::Plugin, d))
        .collect()
}

/// A manifest key that is a path, a list of paths, or absent.
fn declared_paths(manifest: &Value, key: &str) -> Vec<String> {
    match manifest.get(key) {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    }
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bough-foreign-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn skill_at(dir: &Path, name: &str) {
        let folder = dir.join(name);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("SKILL.md"), "---\ndescription: d\n---\nbody").unwrap();
    }

    fn dirs_of(sources: &[SkillSource]) -> Vec<PathBuf> {
        sources.iter().map(|s| s.dir.clone()).collect()
    }

    #[test]
    fn project_skills_are_found_nearest_first_and_stop_at_the_git_root() {
        let root = tmp();
        let repo = root.join("repo");
        let pkg = repo.join("web");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(pkg.join(".agents/skills")).unwrap();
        std::fs::create_dir_all(repo.join(".claude/skills")).unwrap();
        // Above the git root — must never be adopted.
        std::fs::create_dir_all(root.join(".agents/skills")).unwrap();

        let found = dirs_of(&project_sources(&pkg));
        assert_eq!(
            found,
            vec![pkg.join(".agents/skills"), repo.join(".claude/skills")],
            "nearest first, and the walk stops at the git root"
        );
        assert!(found.iter().all(|d| !d.starts_with(root.join(".agents"))));
        assert!(
            found
                .iter()
                .all(|d| d != &root.join(".agents").join("skills")),
            "a directory above the git root is not this project's"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_workspace_with_no_git_root_reads_only_itself() {
        let root = tmp();
        let ws = root.join("a").join("b");
        std::fs::create_dir_all(ws.join(".agents/skills")).unwrap();
        std::fs::create_dir_all(root.join("a").join(".agents/skills")).unwrap();
        // No `.git` anywhere: the walk runs to `/` and falls back to the
        // workspace alone rather than adopting every ancestor.
        let found = dirs_of(&project_sources(&ws));
        assert_eq!(found, vec![ws.join(".agents/skills")]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn installed_claude_plugins_contribute_skills_and_commands() {
        let root = tmp();
        let install = root.join("installs").join("acme");
        std::fs::create_dir_all(install.join("skills")).unwrap();
        std::fs::create_dir_all(install.join("commands")).unwrap();
        skill_at(&install.join("skills"), "review");
        let home = root.join("claude");
        std::fs::create_dir_all(home.join("plugins")).unwrap();
        std::fs::write(
            home.join("plugins").join("installed_plugins.json"),
            serde_json::json!({
                "plugins": {
                    "acme@market": [{"installPath": install.to_string_lossy(), "scope": "user"}]
                }
            })
            .to_string(),
        )
        .unwrap();

        let found = dirs_of(&claude_plugin_sources(&home, &root));
        assert_eq!(
            found,
            vec![install.join("skills"), install.join("commands")]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_project_scoped_plugin_is_taken_only_in_its_own_project() {
        let root = tmp();
        let install = root.join("install");
        std::fs::create_dir_all(install.join("skills")).unwrap();
        let mine = root.join("mine");
        let theirs = root.join("theirs");
        let home = root.join("claude");
        std::fs::create_dir_all(home.join("plugins")).unwrap();
        std::fs::write(
            home.join("plugins").join("installed_plugins.json"),
            serde_json::json!({
                "plugins": {
                    "p@m": [{
                        "installPath": install.to_string_lossy(),
                        "scope": "project",
                        "projectPath": mine.to_string_lossy(),
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(
            dirs_of(&claude_plugin_sources(&home, &mine)),
            vec![install.join("skills")]
        );
        assert!(
            claude_plugin_sources(&home, &theirs).is_empty(),
            "another project's plugin does not follow you into this one"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_or_malformed_registry_contributes_nothing_rather_than_failing() {
        let root = tmp();
        let home = root.join("claude");
        std::fs::create_dir_all(home.join("plugins")).unwrap();
        assert!(claude_plugin_sources(&home, &root).is_empty(), "absent");
        std::fs::write(home.join("plugins").join("installed_plugins.json"), "{ not").unwrap();
        assert!(claude_plugin_sources(&home, &root).is_empty(), "malformed");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_codex_marketplace_resolves_plugin_paths_relative_to_itself() {
        let root = tmp();
        let market = root.join(".agents").join("plugins");
        let plugin = market.join("greeter");
        std::fs::create_dir_all(plugin.join("skills")).unwrap();
        skill_at(&plugin.join("skills"), "greet");
        std::fs::write(
            market.join("marketplace.json"),
            serde_json::json!({
                "plugins": [{"name": "greeter", "source": {"path": "./greeter"}}]
            })
            .to_string(),
        )
        .unwrap();

        let found = dirs_of(&codex_marketplace_sources(&market.join("marketplace.json")));
        assert_eq!(found, vec![plugin.join("skills")]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_marketplace_path_that_escapes_its_root_is_dropped() {
        let root = tmp();
        let market = root.join("market");
        std::fs::create_dir_all(&market).unwrap();
        let outside = root.join("outside");
        std::fs::create_dir_all(outside.join("skills")).unwrap();
        std::fs::write(
            market.join("marketplace.json"),
            serde_json::json!({
                "plugins": [{"source": {"path": "../outside"}}]
            })
            .to_string(),
        )
        .unwrap();
        assert!(
            codex_marketplace_sources(&market.join("marketplace.json")).is_empty(),
            "a checked-in marketplace must not aim the skill loader outside itself"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_manifest_can_redirect_where_a_plugins_skills_live() {
        let root = tmp();
        std::fs::create_dir_all(root.join(".codex-plugin")).unwrap();
        std::fs::create_dir_all(root.join("workflows")).unwrap();
        std::fs::write(
            root.join(".codex-plugin").join("plugin.json"),
            serde_json::json!({"name": "p", "skills": "./workflows/"}).to_string(),
        )
        .unwrap();
        assert_eq!(
            dirs_of(&plugin_skill_dirs(&root)),
            vec![root.join("workflows")]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The shape worktrunk ships: a list, pointing at ONE skill folder.
    #[test]
    fn a_manifest_may_name_the_skill_folder_itself_in_a_list() {
        let root = tmp();
        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        skill_at(&root.join("bundle"), "worktrunk");
        std::fs::write(
            root.join(".claude-plugin").join("plugin.json"),
            serde_json::json!({"name": "p", "skills": ["./bundle/worktrunk"]}).to_string(),
        )
        .unwrap();
        assert_eq!(
            dirs_of(&plugin_skill_dirs(&root)),
            vec![root.join("bundle")],
            "the parent is the source, because that is bough's unit"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_manifest_can_redirect_commands_too_and_the_conventional_dirs_still_count() {
        let root = tmp();
        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(root.join("verbs")).unwrap();
        std::fs::create_dir_all(root.join("skills")).unwrap();
        std::fs::write(
            root.join(".claude-plugin").join("plugin.json"),
            serde_json::json!({"name": "p", "commands": "./verbs"}).to_string(),
        )
        .unwrap();
        assert_eq!(
            dirs_of(&plugin_skill_dirs(&root)),
            vec![root.join("verbs"), root.join("skills")],
            "declared first, then whatever the convention put there"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_directories_that_exist_become_sources() {
        let root = tmp();
        assert!(plugin_skill_dirs(&root).is_empty());
        assert!(user_sources(&root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
