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

use std::path::PathBuf;
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

/// Read the configured files, dedup, render — the WHOLE of what the section says, as a pure-ish
/// function of the config so a test exercises the very path a render takes. `None` ⇒ no section.
pub fn body_of(cfg: &PromptFilesConfig) -> Option<String> {
    let mut found: Vec<(String, String)> = Vec::new();
    for name in &cfg.files {
        let path = cfg.root.join(name);
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

    #[test]
    fn the_body_reads_dedups_truncates_and_is_absent_with_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = PromptFilesConfig {
            root: dir.path().to_path_buf(),
            files: vec!["AGENTS.md".to_string(), "CLAUDE.md".to_string()],
            max_bytes: 65536,
            similarity: 1.0,
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
