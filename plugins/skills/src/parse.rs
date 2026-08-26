//! Invariant: parsing is PURE and refuses LOUDLY. A skill file with no `name` or an empty
//! `triggers` list is not a skill that never fires — it is a misconfiguration, and §0.2 makes a
//! misconfigured row a failure that NAMES THE FILE.

use std::path::Path;

use bough_plugin_projection::SectionId;

/// A skill file: YAML frontmatter + markdown body.
#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub id: SectionId,
    pub name: String,
    pub description: String,
    pub triggers: Vec<String>,
    pub body: String,
}

/// PURE: parse, and refuse loudly on a missing `name` or empty `triggers`. WP-7.
pub fn parse_skill(path: &Path, text: &str) -> Result<Skill, SkillError> {
    let _ = (path, text);
    todo!("WP-7")
}

/// PURE: does this request mention the skill? Case-insensitive WHOLE-WORD match of any trigger
/// against the scanned text. WP-7.
pub fn mentioned(skill: &Skill, scanned: &str) -> bool {
    let _ = (skill, scanned);
    todo!("WP-7")
}

/// What a skill file goes wrong as. Every variant names the file.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("skill `{path}`: no YAML frontmatter")]
    NoFrontmatter { path: String },
    #[error("skill `{path}`: frontmatter is not valid YAML: {detail}")]
    BadFrontmatter { path: String, detail: String },
    #[error("skill `{path}`: a skill needs a `name`")]
    NoName { path: String },
    #[error("skill `{path}`: a skill needs at least one trigger, or it can never inject")]
    NoTriggers { path: String },
    #[error("skill `{path}`: {bytes} bytes exceeds `max_bytes` ({max})")]
    TooBig {
        path: String,
        bytes: usize,
        max: usize,
    },
    #[error("skill `{path}`: {detail}")]
    Io { path: String, detail: String },
}
