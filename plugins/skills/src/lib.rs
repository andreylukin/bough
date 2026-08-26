//! Invariant: an UNMENTIONED SKILL CONTRIBUTES NOTHING. Each skill child registers one projection
//! section whose `render` returns `Ok(None)` unless the request mentions one of its triggers, so a
//! skill that is not asked for does not appear at all and costs no budget.
//!
//! The section honours `SectionRequest::as_of` — a contributed section that ignores it stops past
//! requests reproducing (the rule is in `projection/src/section.rs` and applies here).
//!
//! Ties break by [`SectionId`], never by load order (the P1-D8 rule), so `max_injected` is
//! deterministic.

pub mod invariant;
pub mod parse;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_projection::SectionId;

pub use parse::{mentioned, parse_skill, Skill, SkillError};

/// The catalog name of the host row.
pub const PLUGIN_NAME: &str = "skills";
/// The catalog name of the per-file CHILD row.
pub const SKILL_PLUGIN_NAME: &str = "skill";

/// The host row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkillsConfig {
    pub dir: PathBuf,
    pub glob: String,
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

/// PURE: the section id one skill file registers under. WP-7.
pub fn section_id(skill: &Skill) -> SectionId {
    let _ = skill;
    todo!("WP-7: `skill:<name>`")
}

/// The host row.
pub struct SkillsHostPlugin;

#[async_trait::async_trait]
impl Plugin for SkillsHostPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = SkillsConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["projection", "ledger"])
            .union(&bough_kernel::Inject::optional(["commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-7: `max_bytes > 0`, `max_injected > 0`, `scan_steps > 0`")
    }

    /// Mount one child entry per skill file, and (when `watch`) a notify+debouncer watch that
    /// reconciles EXACTLY the changed child. WP-7.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-7")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
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
        let _ = cfg;
        todo!("WP-7")
    }

    /// Parse the file — refusing LOUDLY, so the child entry FAILS naming the file — then register
    /// ONE section: `Position { slot: Slot::Tiers, place: Place::After }`, `SectionScope::Global`,
    /// `DropPriority::Fine`. WP-7.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-7")
    }

    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}

bough_kernel::register_plugin!(SkillsHostPlugin);
bough_kernel::register_plugin!(SkillPlugin);
