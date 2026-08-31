//! Invariant: this row contributes ONE global projection section, read fresh from the workspace's
//! prompt files at every render — an edit to AGENTS.md reaches the next wake with no reload — and
//! DUPLICATE CONTENT IS INJECTED ONCE: a paragraph two files both state appears in the first
//! file's voice with the later copies dropped (`dedup.rs`). The section cites no ledger rows
//! because the files are not ledgered; like the boundary block, it deliberately does not vary
//! with `as_of` — a re-assembled past request reads today's files, and the section says which.
//!
//! §5's contributed-section seam carries it; `bough-plugin-boundary-instructions` is the shape
//! this row copies.

pub mod dedup;
pub mod invariant;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_projection::{
    DropPriority, Place, Position, Projection, SectionBody, SectionCites, SectionId, SectionRender,
    SectionRequest, SectionScope, SectionSpec, Slot,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "prompt-files";

/// The contributed section's identity.
pub fn section_id() -> SectionId {
    SectionId::new("prompt-files")
}

/// The row's config. Every deployment-varying value is here (§0.2).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PromptFilesConfig {
    /// The workspace the files live in. Relative paths resolve against the process cwd, the same
    /// rule `tools.baseline.root` follows.
    pub root: PathBuf,
    /// The files, in PRECEDENCE order: a duplicated paragraph survives in the earliest file that
    /// states it. Missing files are simply absent, never an error.
    pub files: Vec<String>,
    /// Per-file size cap; a larger file is truncated with a marker, because one runaway prompt
    /// file must not eat the projection.
    pub max_bytes: u64,
    /// The near-duplicate threshold (Sorensen-Dice over normalized text). `1.0` = exact-only.
    pub similarity: f64,
    /// Also read `~/.claude/<file>` for each configured file, ahead of everything else: the
    /// user's global instructions (Claude Code's `~/.claude/CLAUDE.md` rule).
    #[serde(default)]
    pub home: bool,
    /// Read `<dir>/<file>` for every ancestor from the filesystem root down to `root`, outermost
    /// first, then `<root>/.claude/<file>` (Claude Code's walk-up rule). Off, the row reads
    /// `<root>/<file>` alone — the original behavior.
    #[serde(default)]
    pub walk_up: bool,
}

/// The renderer: read, dedup, render — all at request time.
struct PromptFilesSection {
    cfg: Arc<PromptFilesConfig>,
}

#[async_trait::async_trait]
impl SectionRender for PromptFilesSection {
    async fn render(
        &self,
        _req: &SectionRequest,
    ) -> Result<Option<SectionBody>, bough_plugin_projection::ProjectionError> {
        Ok(body_of(&self.cfg).map(|body| SectionBody {
            title: "workspace instructions".to_string(),
            body,
            cites: SectionCites::default(),
        }))
    }
}

/// Read the discovered files, dedup, render — the WHOLE of what the section says. `None` ⇒ no
/// section. The real render reads against the process's `$HOME`; tests pass their own.
pub fn body_of(cfg: &PromptFilesConfig) -> Option<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    body_with_home(cfg, home.as_deref())
}

/// Every path a configured file may live at, in PRECEDENCE order (the earliest voice wins the
/// dedup): the home tree, then each ancestor outermost-first, then `<root>/.claude`. With both
/// flags off this is exactly `root.join(name)` — the original rule. A path visited twice (a
/// `root` that IS the home tree) is listed once.
pub fn discover(cfg: &PromptFilesConfig, home: Option<&Path>) -> Vec<(String, PathBuf)> {
    let root = cfg.root.canonicalize().unwrap_or_else(|_| cfg.root.clone());
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut push = |label: String, path: PathBuf| {
        if seen.insert(path.canonicalize().unwrap_or_else(|_| path.clone())) {
            out.push((label, path));
        }
    };
    for name in &cfg.files {
        if cfg.home {
            if let Some(h) = home {
                let path = h.join(".claude").join(name);
                push(format!("~/.claude/{name}"), path);
            }
        }
        if cfg.walk_up {
            let mut chain: Vec<&Path> = root.ancestors().collect();
            chain.reverse();
            for dir in chain {
                push(dir.join(name).display().to_string(), dir.join(name));
            }
            let dot = root.join(".claude").join(name);
            push(dot.display().to_string(), dot);
        } else {
            push(name.clone(), cfg.root.join(name));
        }
    }
    out
}

/// [`body_of`] against an explicit home, so a test never reads the developer's real `~/.claude`.
pub fn body_with_home(cfg: &PromptFilesConfig, home: Option<&Path>) -> Option<String> {
    let mut found: Vec<(String, String)> = Vec::new();
    for (name, path) in discover(cfg, home) {
        match std::fs::read_to_string(&path) {
            Ok(mut text) => {
                if text.len() as u64 > cfg.max_bytes {
                    let mut cut = cfg.max_bytes as usize;
                    while cut > 0 && !text.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    text.truncate(cut);
                    text.push_str("\n\n(truncated: the file is over the configured cap)");
                }
                found.push((name.clone(), text));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                // A file that exists but cannot be read is said, not hidden: silently missing
                // instructions are the worst kind of missing.
                tracing::warn!(file = %path.display(), error = %e, "prompt file unreadable");
            }
        }
    }
    if found.is_empty() {
        return None;
    }
    let body = dedup::render_body(&dedup::dedup(&found, cfg.similarity));
    if body.is_empty() {
        return None;
    }
    Some(body)
}

/// The one spec this row contributes: global, after the identity band (beside the boundary
/// block), droppable at the COARSE rung — instructions matter, a buildable wake matters more.
pub fn section_spec(cfg: Arc<PromptFilesConfig>) -> SectionSpec {
    SectionSpec {
        id: section_id(),
        position: Position {
            slot: Slot::Identity,
            place: Place::After,
        },
        scope: SectionScope::Global,
        agent: None,
        priority: DropPriority::Coarse,
        render: Arc::new(PromptFilesSection { cfg }),
    }
}

/// The row.
pub struct PromptFilesPlugin;

#[async_trait::async_trait]
impl Plugin for PromptFilesPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = PromptFilesConfig;

    fn inject() -> Inject {
        Inject::required(["projection"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        if cfg.root.as_os_str().is_empty() {
            return reject("root must name a directory".to_string());
        }
        if cfg.files.is_empty() {
            return reject("files must name at least one file".to_string());
        }
        if cfg.files.iter().any(|f| f.trim().is_empty()) {
            return reject("a file entry must not be empty".to_string());
        }
        if cfg.max_bytes == 0 {
            return reject("max_bytes must be > 0".to_string());
        }
        if !(cfg.similarity > 0.0 && cfg.similarity <= 1.0) {
            return reject("similarity must be in (0.0, 1.0]".to_string());
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let projection = ctx
            .get::<Projection>()
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
        // A REGISTRATION IS AN EFFECT: unloading this row removes the section.
        projection.section(&ctx, section_spec(cfg)).await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(PromptFilesPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PromptFilesConfig {
        PromptFilesConfig {
            root: PathBuf::from("."),
            files: vec!["AGENTS.md".to_string()],
            max_bytes: 65536,
            similarity: 0.85,
            home: false,
            walk_up: false,
        }
    }

    #[test]
    fn validate_refuses_the_degenerate_values() {
        assert!(PromptFilesPlugin::validate(&cfg()).is_ok());
        assert!(PromptFilesPlugin::validate(&PromptFilesConfig {
            files: vec![],
            ..cfg()
        })
        .is_err());
        assert!(PromptFilesPlugin::validate(&PromptFilesConfig {
            files: vec![" ".to_string()],
            ..cfg()
        })
        .is_err());
        assert!(PromptFilesPlugin::validate(&PromptFilesConfig {
            max_bytes: 0,
            ..cfg()
        })
        .is_err());
        assert!(PromptFilesPlugin::validate(&PromptFilesConfig {
            similarity: 0.0,
            ..cfg()
        })
        .is_err());
        assert!(PromptFilesPlugin::validate(&PromptFilesConfig {
            similarity: 1.5,
            ..cfg()
        })
        .is_err());
    }

    /// Drivability §5: the Claude Code discovery order — home tree first, then the ancestors
    /// outermost-first, then `<root>/.claude` — and the flags off keep the original flat rule.
    #[test]
    fn discovery_walks_home_then_ancestors_then_the_dot_claude_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("a").join("b");
        std::fs::create_dir_all(&root).unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let c = PromptFilesConfig {
            root: root.clone(),
            files: vec!["CLAUDE.md".to_string()],
            max_bytes: 65536,
            similarity: 1.0,
            home: true,
            walk_up: true,
        };
        let paths: Vec<PathBuf> = discover(&c, Some(&home)).into_iter().map(|(_, p)| p).collect();
        let root = root.canonicalize().unwrap();
        assert_eq!(paths[0], home.join(".claude").join("CLAUDE.md"));
        let inner = paths
            .iter()
            .position(|p| *p == root.join("CLAUDE.md"))
            .expect("root itself is walked");
        let outer = paths
            .iter()
            .position(|p| *p == root.parent().unwrap().join("CLAUDE.md"))
            .expect("the parent is walked");
        assert!(outer < inner, "outermost first: {paths:?}");
        assert_eq!(
            paths.last().unwrap(),
            &root.join(".claude").join("CLAUDE.md")
        );
        // Flags off: the original flat rule, exactly.
        let flat = PromptFilesConfig {
            home: false,
            walk_up: false,
            ..c.clone()
        };
        assert_eq!(
            discover(&flat, Some(&home))
                .into_iter()
                .map(|(_, p)| p)
                .collect::<Vec<_>>(),
            vec![flat.root.join("CLAUDE.md")]
        );
        // The walked-up bodies land in the section, global voice first.
        std::fs::write(home.join(".claude/CLAUDE.md"), "Be terse.").unwrap();
        std::fs::write(root.join("CLAUDE.md"), "Never push to main.").unwrap();
        let body = body_with_home(&c, Some(&home)).expect("a section");
        let global = body.find("Be terse.").expect("global instructions present");
        let project = body.find("Never push to main.").expect("project instructions present");
        assert!(global < project, "{body}");
    }

    #[test]
    fn the_body_reads_dedups_truncates_and_is_absent_with_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = PromptFilesConfig {
            root: dir.path().to_path_buf(),
            files: vec!["AGENTS.md".to_string(), "CLAUDE.md".to_string()],
            max_bytes: 65536,
            similarity: 1.0,
            home: false,
            walk_up: false,
        };
        assert_eq!(body_of(&c), None, "no files, no section");
        std::fs::write(dir.path().join("AGENTS.md"), "Be brief.\n\nAsk first.").unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "Be brief.\n\nNever push.").unwrap();
        let body = body_of(&c).expect("a section");
        assert!(body.contains("## AGENTS.md\nBe brief."), "{body}");
        assert!(body.contains("Never push."), "{body}");
        assert!(
            body.contains("(1 block(s) omitted: already stated above)"),
            "{body}"
        );
        c.max_bytes = 8;
        let body = body_of(&c).expect("a section");
        assert!(body.contains("(truncated"), "{body}");
    }
}
