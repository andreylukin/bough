//! Invariant: an UNMENTIONED SKILL CONTRIBUTES NOTHING. Each skill child registers one projection
//! section whose `render` returns `Ok(None)` unless the request mentions one of its triggers, so a
//! skill that is not asked for does not appear at all and costs no budget.
//!
//! The section honours `SectionRequest::as_of` — a contributed section that ignores it stops past
//! requests reproducing (the rule is in `projection/src/section.rs` and applies here).
//!
//! Ties break by [`SectionId`], never by load order (the P1-D8 rule), so `max_injected` is
//! deterministic.
//!
//! HOT RELOAD is dispose-then-mount of EXACTLY ONE child fiber: the digest of the file is a config
//! field, so an edit changes one child's config and nothing else. Disposal goes through
//! `Kernel::runtime().dispose(uid)`, which is the public API a plugin has for the child it mounted
//! (see `docs/track-b-merge-notes.md`).

pub mod invariant;
pub mod parse;
pub mod registry;
pub mod section;
pub mod tool;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Entry, InvariantSpec, Plugin, PluginError};
use bough_plugin_projection::{
    DropPriority, Projection, ProjectionHandle, SectionId, SectionScope, SectionSpec,
};

pub use parse::{mentioned, parse_skill, Skill, SkillError};

/// The catalog name of the host row.
pub const PLUGIN_NAME: &str = "skills";
/// The catalog name of the per-file CHILD row.
pub const SKILL_PLUGIN_NAME: &str = "skill";

/// How often the reload task looks up from the watch channel to see whether it has been halted.
const RELOAD_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// The host row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkillsConfig {
    pub dir: PathBuf,
    pub glob: String,
    /// Extra discovery roots, walked for `SKILL.md` files at any depth (bounded): the Claude Code
    /// layouts — `~/.claude/skills/<name>/SKILL.md` and the installed-plugin trees under
    /// `~/.claude/plugins` (drivability §5). A missing root is empty, never an error.
    #[serde(default)]
    pub roots: Vec<PathBuf>,
    /// Skill names to mount, exactly ([`skill_name_of`]); empty = every discovered skill. The
    /// patch-level way to turn ON just the skills you want (drivability §5).
    #[serde(default)]
    pub only: Vec<String>,
    /// Skill names to skip. Applied after `only`.
    #[serde(default)]
    pub except: Vec<String>,
    pub watch: bool,
    pub debounce_ms: u64,
    pub max_bytes: usize,
    /// At most this many skills inject into one request; ties break by [`SectionId`].
    pub max_injected: usize,
    /// How much of the verbatim tail + unconsumed mail the trigger scan reads.
    pub scan_steps: usize,
}

/// One skill file's child config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkillConfig {
    pub path: PathBuf,
    /// sha256 of the file; a change here reloads exactly this one child.
    pub digest: String,
    pub host: SkillsConfig,
}

/// PURE: the section id one skill file registers under.
pub fn section_id(skill: &Skill) -> SectionId {
    skill.id.clone()
}

/// PURE: sha256 of a file's bytes, hex. The child config field an edit changes.
pub fn digest_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// PURE: does `name` match `glob`? Only the two shapes a skill pool needs: `*` and `*.<ext>`, or a
/// literal name. Anything else is refused by [`SkillsHostPlugin::validate`], so this never guesses.
pub fn glob_matches(glob: &str, name: &str) -> bool {
    match glob.strip_prefix('*') {
        Some("") => true,
        Some(suffix) => name.ends_with(suffix) && name.len() > suffix.len(),
        None => glob == name,
    }
}

/// PURE: the child entry one skill file mounts as. `id` is `<parent>.<file stem>` — and a
/// `SKILL.md` is named for its DIRECTORY (`skills/review/SKILL.md` ⇒ `review`), because every
/// such file shares the same stem. Two roots naming one skill collide loudly at mount (§0.2).
pub fn child_entry(parent: &str, path: &Path, digest: &str, host: &SkillsConfig) -> Entry {
    let stem = skill_name_of(path);
    Entry {
        id: bough_kernel::EntryId::new(format!("{parent}.{stem}")),
        plugin: Some(SKILL_PLUGIN_NAME.to_string()),
        config: serde_yaml::to_value(SkillConfig {
            path: path.to_path_buf(),
            digest: digest.to_string(),
            host: host.clone(),
        })
        .expect("SkillConfig serializes"),
        disabled: Default::default(),
        isolate: Default::default(),
        inject: Default::default(),
        group: Vec::new(),
        include: None,
    }
}

/// Every file in `dir` matching `glob`, sorted, with its digest. IO, deliberately separate from
/// the pure parts above.
pub fn scan_dir(dir: &Path, glob: &str) -> Result<Vec<(PathBuf, String)>, std::io::Error> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // A pool directory that does not exist yet is EMPTY, not a boot failure: the host still
        // activates and the watch picks the first file up.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !glob_matches(glob, &name) {
            continue;
        }
        let bytes = std::fs::read(entry.path())?;
        out.push((entry.path(), digest_of(&bytes)));
    }
    out.sort();
    Ok(out)
}

/// PURE: the name a skill file goes by — the file stem, except a `SKILL.md`, which is named for
/// its DIRECTORY. The child entry id, the `only`/`except` toggles and the catalog all use it.
pub fn skill_name_of(path: &Path) -> String {
    if path.file_name().is_some_and(|n| n == "SKILL.md") {
        path.parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "skill".to_string())
    } else {
        path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "skill".to_string())
    }
}

/// How deep [`scan_root`] walks. Deep enough for the installed-plugin trees
/// (`plugins/marketplaces/<m>/<p>/skills/<name>/SKILL.md`); a bound, so a symlink cycle ends.
const ROOT_WALK_DEPTH: usize = 8;

/// Every `SKILL.md` under `root`, at any depth up to [`ROOT_WALK_DEPTH`], sorted, with its
/// digest. Symlinks are followed (a skill directory is often a symlink); hidden directories and
/// `node_modules` are not descended. A missing root is EMPTY, not an error — the same rule as
/// [`scan_dir`].
pub fn scan_root(root: &Path) -> Result<Vec<(PathBuf, String)>, std::io::Error> {
    let mut out = Vec::new();
    walk(root, ROOT_WALK_DEPTH, &mut out)?;
    out.sort();
    return Ok(out);

    fn walk(
        dir: &Path,
        depth: usize,
        out: &mut Vec<(PathBuf, String)>,
    ) -> Result<(), std::io::Error> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            // `metadata`, not `file_type`: a symlinked skill directory must count as a directory.
            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_file() {
                if entry.file_name() == "SKILL.md" {
                    let bytes = std::fs::read(&path)?;
                    out.push((path, digest_of(&bytes)));
                }
            } else if meta.is_dir() && depth > 0 {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') || name == "node_modules" {
                    continue;
                }
                walk(&path, depth - 1, out)?;
            }
        }
        Ok(())
    }
}

/// The whole discovered set: the pool directory's flat files AND its `SKILL.md` walk (so
/// `$BOUGH_HOME/skills` carries both layouts), then every root's `SKILL.md` walk. A path found
/// twice is listed once.
pub fn scan_all(cfg: &SkillsConfig) -> Result<Vec<(PathBuf, String)>, std::io::Error> {
    let mut out = scan_dir(&cfg.dir, &cfg.glob)?;
    out.extend(scan_root(&cfg.dir)?);
    for root in &cfg.roots {
        out.extend(scan_root(root)?);
    }
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    out.retain(|(p, _)| {
        let name = skill_name_of(p);
        (cfg.only.is_empty() || cfg.only.contains(&name)) && !cfg.except.contains(&name)
    });
    Ok(out)
}

/// The host row.
pub struct SkillsHostPlugin;

#[async_trait::async_trait]
impl Plugin for SkillsHostPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = SkillsConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["projection", "ledger"])
            .union(&bough_kernel::Inject::optional(["commands", "tools"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let bad = |detail: String| ConfigError::Rejected { detail };
        if cfg.max_bytes == 0 {
            return Err(bad("max_bytes: a skill of zero bytes is unreadable".into()));
        }
        if cfg.max_injected == 0 {
            return Err(bad(
                "max_injected: zero means no skill can ever inject; disable the row instead".into(),
            ));
        }
        if cfg.scan_steps == 0 {
            return Err(bad(
                "scan_steps: zero means nothing is scanned and no trigger can fire".into(),
            ));
        }
        if cfg.glob.is_empty() {
            return Err(bad("glob: empty".into()));
        }
        if cfg.glob.matches('*').count() > 1 || cfg.glob.contains('?') {
            return Err(bad(format!(
                "glob: `{}` — only `*`, `*.<ext>` and a literal name are understood",
                cfg.glob
            )));
        }
        if cfg.watch && cfg.debounce_ms == 0 {
            return Err(bad(
                "debounce_ms: a watch with no debounce refires per write".into(),
            ));
        }
        Ok(())
    }

    /// Mount one child entry per skill file, and (when `watch`) a notify+debouncer watch that
    /// reconciles EXACTLY the changed child.
    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let files = scan_all(&cfg)
            .map_err(|e| PluginError::new(entry.clone(), anyhow::Error::new(e)))?;

        // The catalog and the loader (drivability §5): ONE section listing every skill in the
        // pool by name + description, and a `skill` tool that loads one body as a ledgered
        // `tool/result` — the model chooses, instead of waiting for a trigger word. The tool
        // registers only where a `tools` row is composed.
        let pool = registry::pool(&cfg.dir);
        {
            let projection = ctx
                .get::<Projection>()
                .map_err(|e| PluginError::new(entry.clone(), e))?;
            projection
                .section(&ctx, section::catalog_spec(Arc::clone(&pool)))
                .await?;
        }
        if let Ok(tools) = ctx.get::<bough_plugin_tools::Tools>() {
            tools.register(&ctx, tool::spec(Arc::clone(&pool))).await?;
        }

        let mounted: Arc<parking_lot::Mutex<BTreeMap<PathBuf, (String, bough_kernel::FiberUid)>>> =
            Arc::new(parking_lot::Mutex::new(BTreeMap::new()));
        for (path, digest) in files {
            let child = child_entry(entry.as_str(), &path, &digest, &cfg);
            let handle = ctx
                .mount(child)
                .await
                .map_err(|e| PluginError::new(entry.clone(), e))?;
            mounted.lock().insert(path, (digest, handle.uid()));
        }

        if cfg.watch {
            let ctx2 = ctx.clone();
            let cfg2 = cfg.clone();
            let entry2 = entry.clone();
            let entry3 = entry.clone();
            let mounted2 = Arc::clone(&mounted);
            // A channel, so the notify thread never touches the kernel and the reconcile runs on
            // the runtime like any other effect.
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
            let debounce = std::time::Duration::from_millis(cfg.debounce_ms);
            let dir = cfg.dir.clone();
            let roots = cfg.roots.clone();
            let watch = ctx
                .effect(move |e| async move {
                    let _ = std::fs::create_dir_all(&dir);
                    let mut debouncer = notify_debouncer_full::new_debouncer(
                        debounce,
                        None,
                        move |res: notify_debouncer_full::DebounceEventResult| {
                            if res.is_ok() {
                                let _ = tx.send(());
                            }
                        },
                    )
                    .map_err(|err| PluginError::new(entry2.clone(), anyhow::Error::new(err)))?;
                    // Recursive: the `<name>/SKILL.md` layout puts edits a level (or more) down.
                    debouncer
                        .watch(&dir, notify::RecursiveMode::Recursive)
                        .map_err(|err| PluginError::new(entry2.clone(), anyhow::Error::new(err)))?;
                    for root in &roots {
                        // A missing root is empty, not an error — but it cannot be watched; it is
                        // picked up on the next restart or on a `dir` event's reconcile.
                        if let Err(err) = debouncer.watch(root, notify::RecursiveMode::Recursive) {
                            tracing::warn!(root = %root.display(), error = %err, "skills root not watched");
                        }
                    }
                    e.defer_sync(move || drop(debouncer));
                    Ok(())
                })
                .await?;
            drop(watch);

            ctx.effect_spawn(move |e| async move {
                // A bare `rx.recv().await` never reaches a halt checkpoint, so the fiber cannot
                // settle on unload and the kernel times it out. Poll the channel instead, and
                // check the halt flag between polls (§0.2: unload leaves no trace, promptly).
                loop {
                    if e.is_halted() {
                        return Ok(());
                    }
                    match tokio::time::timeout(RELOAD_POLL, rx.recv()).await {
                        Ok(Some(())) => {
                            if let Err(err) = reconcile(&ctx2, &entry3, &cfg2, &mounted2).await {
                                tracing::warn!(dir = %cfg2.dir.display(), error = %err, "skills reload failed");
                            }
                        }
                        // The sender is gone with the watch: nothing more can arrive.
                        Ok(None) => return Ok(()),
                        Err(_) => continue,
                    }
                }
            });
        }
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

/// Bring the mounted child set in line with the directory: dispose exactly the children whose file
/// changed or vanished, mount exactly the new and changed ones. Public so a test can drive it
/// without racing a filesystem watcher.
pub async fn reconcile(
    ctx: &Context,
    entry: &bough_kernel::EntryId,
    cfg: &SkillsConfig,
    mounted: &Arc<parking_lot::Mutex<BTreeMap<PathBuf, (String, bough_kernel::FiberUid)>>>,
) -> Result<(), anyhow::Error> {
    let files = scan_all(cfg)?;
    let now: BTreeMap<PathBuf, String> = files.into_iter().collect();
    let before: BTreeMap<PathBuf, (String, bough_kernel::FiberUid)> = mounted.lock().clone();

    let kernel = ctx
        .kernel()
        .ok_or_else(|| anyhow::anyhow!("skills: the host fiber has no kernel to mount into"))?;

    // Gone or changed: dispose that ONE child.
    for (path, (digest, uid)) in &before {
        if now.get(path) != Some(digest) {
            kernel.runtime().dispose(*uid).await;
            mounted.lock().remove(path);
        }
    }
    // New or changed: mount that ONE child.
    for (path, digest) in &now {
        if mounted.lock().contains_key(path) {
            continue;
        }
        let child = child_entry(entry.as_str(), path, digest, cfg);
        let handle = ctx.mount(child).await?;
        mounted
            .lock()
            .insert(path.clone(), (digest.clone(), handle.uid()));
    }
    Ok(())
}

/// One skill file's child row.
pub struct SkillPlugin;

#[async_trait::async_trait]
impl Plugin for SkillPlugin {
    const NAME: &'static str = SKILL_PLUGIN_NAME;
    type Config = SkillConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["projection", "ledger"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        if cfg.digest.is_empty() {
            return Err(ConfigError::Rejected {
                detail: format!(
                    "skill `{}`: no digest — the host mints one per file",
                    cfg.path.display()
                ),
            });
        }
        SkillsHostPlugin::validate(&cfg.host)
    }

    /// Parse the file — refusing LOUDLY, so the child entry FAILS naming the file — then register
    /// ONE section.
    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let fail = |e: SkillError| PluginError::new(entry.clone(), anyhow::Error::new(e));

        let bytes = std::fs::read(&cfg.path).map_err(|e| {
            fail(SkillError::Io {
                path: cfg.path.display().to_string(),
                detail: e.to_string(),
            })
        })?;
        if bytes.len() > cfg.host.max_bytes {
            return Err(fail(SkillError::TooBig {
                path: cfg.path.display().to_string(),
                bytes: bytes.len(),
                max: cfg.host.max_bytes,
            }));
        }
        let text = String::from_utf8(bytes).map_err(|e| {
            fail(SkillError::Io {
                path: cfg.path.display().to_string(),
                detail: e.to_string(),
            })
        })?;
        let skill = Arc::new(parse_skill(&cfg.path, &text).map_err(fail)?);

        // The pool registration is an effect, so an unloaded skill leaves no trace on the cap.
        let pool = registry::pool(&cfg.host.dir);
        let p = Arc::clone(&pool);
        let s = Arc::clone(&skill);
        ctx.effect(move |e| async move {
            let undo = p.insert(s);
            e.defer_sync(undo);
            Ok(())
        })
        .await?;

        let projection = ctx
            .get::<Projection>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        ProjectionHandle(projection.0.clone())
            .section(
                &ctx,
                SectionSpec {
                    id: section_id(&skill),
                    position: section::POSITION,
                    scope: SectionScope::Global,
                    agent: None,
                    priority: DropPriority::Fine,
                    render: Arc::new(section::SkillSection {
                        skill,
                        pool,
                        scan_steps: cfg.host.scan_steps,
                        max_injected: cfg.host.max_injected,
                    }),
                },
            )
            .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}

bough_kernel::register_plugin!(SkillsHostPlugin);
bough_kernel::register_plugin!(SkillPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SkillsConfig {
        SkillsConfig {
            dir: PathBuf::from("/skills"),
            glob: "*.md".into(),
            roots: vec![],
            only: vec![],
            except: vec![],
            watch: true,
            debounce_ms: 400,
            max_bytes: 65536,
            max_injected: 3,
            scan_steps: 40,
        }
    }

    #[test]
    fn the_glob_understands_star_star_ext_and_a_literal() {
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("*.md", "review.md"));
        assert!(
            !glob_matches("*.md", ".md"),
            "a bare extension is not a name"
        );
        assert!(!glob_matches("*.md", "review.txt"));
        assert!(glob_matches("review.md", "review.md"));
        assert!(!glob_matches("review.md", "other.md"));
    }

    #[test]
    fn validate_refuses_the_configs_that_could_never_fire() {
        for bad in [
            SkillsConfig {
                max_bytes: 0,
                ..cfg()
            },
            SkillsConfig {
                max_injected: 0,
                ..cfg()
            },
            SkillsConfig {
                scan_steps: 0,
                ..cfg()
            },
            SkillsConfig {
                glob: String::new(),
                ..cfg()
            },
            SkillsConfig {
                glob: "*a*".into(),
                ..cfg()
            },
            SkillsConfig {
                debounce_ms: 0,
                ..cfg()
            },
        ] {
            assert!(SkillsHostPlugin::validate(&bad).is_err(), "{bad:?}");
        }
        assert!(SkillsHostPlugin::validate(&cfg()).is_ok());
    }

    #[test]
    fn a_child_entry_is_named_for_the_file_and_carries_the_digest() {
        let e = child_entry("skills", Path::new("/skills/review.md"), "abc", &cfg());
        assert_eq!(e.id.as_str(), "skills.review");
        assert_eq!(e.plugin.as_deref(), Some("skill"));
        let parsed: SkillConfig = serde_yaml::from_value(e.config).expect("round-trips");
        assert_eq!(parsed.digest, "abc");
        assert_eq!(parsed.path, PathBuf::from("/skills/review.md"));
    }

    #[test]
    fn an_edit_changes_exactly_one_childs_config() {
        let a = child_entry("skills", Path::new("/skills/a.md"), "d1", &cfg());
        let a2 = child_entry("skills", Path::new("/skills/a.md"), "d2", &cfg());
        let b = child_entry("skills", Path::new("/skills/b.md"), "d3", &cfg());
        assert_eq!(a.id, a2.id);
        assert_ne!(a.config, a2.config);
        assert_ne!(a.id, b.id);
    }

    /// Drivability §5: a `SKILL.md` child is named for its DIRECTORY, so every Claude Code skill
    /// does not collide on the stem `SKILL`.
    #[test]
    fn a_skill_md_child_is_named_for_its_directory() {
        let e = child_entry("skills", Path::new("/roots/review/SKILL.md"), "abc", &cfg());
        assert_eq!(e.id.as_str(), "skills.review");
    }

    /// Drivability §5: the root walk finds `SKILL.md` at both Claude Code depths — a personal
    /// `skills/<name>/` and an installed-plugin tree — follows a symlinked skill directory, and
    /// ignores everything not named `SKILL.md`.
    #[test]
    fn scan_root_finds_nested_skill_md_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("alpha")).unwrap();
        std::fs::write(root.join("alpha/SKILL.md"), "a").unwrap();
        std::fs::create_dir_all(root.join("marketplaces/m/skills/beta")).unwrap();
        std::fs::write(root.join("marketplaces/m/skills/beta/SKILL.md"), "b").unwrap();
        std::fs::write(root.join("notes.md"), "not a skill").unwrap();
        let elsewhere = dir.path().join("elsewhere/gamma");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("SKILL.md"), "g").unwrap();
        std::fs::create_dir_all(root.join("linked")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("linked/gamma")).unwrap();
        let scan_within = |sub: &Path| {
            scan_root(sub)
                .expect("walks")
                .into_iter()
                .map(|(p, _)| p)
                .collect::<Vec<_>>()
        };
        let alpha_root = scan_within(&root.join("alpha"));
        assert_eq!(alpha_root, vec![root.join("alpha/SKILL.md")]);
        let all = scan_within(root);
        assert!(all.contains(&root.join("alpha/SKILL.md")), "{all:?}");
        assert!(
            all.contains(&root.join("marketplaces/m/skills/beta/SKILL.md")),
            "{all:?}"
        );
        assert!(
            all.contains(&root.join("linked/gamma/SKILL.md")),
            "a symlinked skill directory is followed: {all:?}"
        );
        assert!(
            !all.iter().any(|p| p.ends_with("notes.md")),
            "only SKILL.md files: {all:?}"
        );
        assert_eq!(
            scan_root(&root.join("missing")).expect("empty, not an error"),
            vec![]
        );
    }

    /// Drivability §5: `$BOUGH_HOME/skills` carries BOTH layouts — flat `<name>.md` and
    /// `<name>/SKILL.md` — and the `only`/`except` toggles pick skills by name across them.
    #[test]
    fn scan_all_reads_both_pool_layouts_and_honours_the_toggles() {
        let dir = tempfile::tempdir().unwrap();
        let pool = dir.path();
        std::fs::write(pool.join("alpha.md"), "a").unwrap();
        std::fs::create_dir_all(pool.join("beta")).unwrap();
        std::fs::write(pool.join("beta/SKILL.md"), "b").unwrap();
        let base = SkillsConfig {
            dir: pool.to_path_buf(),
            ..cfg()
        };
        let names = |c: &SkillsConfig| {
            scan_all(c)
                .expect("scans")
                .into_iter()
                .map(|(p, _)| skill_name_of(&p))
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&base), vec!["alpha", "beta"]);
        assert_eq!(
            names(&SkillsConfig {
                only: vec!["beta".into()],
                ..base.clone()
            }),
            vec!["beta"]
        );
        assert_eq!(
            names(&SkillsConfig {
                except: vec!["beta".into()],
                ..base.clone()
            }),
            vec!["alpha"]
        );
    }

    #[test]
    fn a_missing_pool_directory_is_empty_not_a_failure() {
        let dir = std::env::temp_dir().join("bough-skills-does-not-exist-9d2f1");
        assert_eq!(
            scan_dir(&dir, "*.md").expect("empty, not an error").len(),
            0
        );
    }
}
