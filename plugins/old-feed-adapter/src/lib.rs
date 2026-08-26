//! THIS CRATE IS SCHEDULED FOR DELETION IN PHASE 6, and `disabled: true` in the bundle patch is
//! its off switch (§14).
//!
//! Invariant: delivery is AT-LEAST-ONCE WITH A REF GUARD, so a restart duplicates nothing. Each
//! batch is filtered against the ledger's existing `mail/delivered` refs, then delivered through
//! `Agent::deliver` (which writes the cited step and the splice as a pair), then watermarked — so
//! a crash between the append and the watermark write cannot duplicate: the ref guard catches it
//! on restart (V7).
//!
//! And the rule that is easiest to get wrong: `command_history` / `command_tags` are COMPETENCE
//! MEMORY exposed through a priming query. They are never mail, never a step, and never a
//! projection section in this phase (§14, §17).

pub mod invariant;
pub mod jungler;
pub mod state;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError, ServiceKey};
use bough_plugin_ledger::Cite;
use chrono::{DateTime, Utc};

pub use jungler::{probe, FeedProbe};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "old-feed-adapter";

/// The `old_feed` service key.
pub struct OldFeed;

impl ServiceKey for OldFeed {
    type Value = OldFeedHandle;
    const NAME: &'static str = "old_feed";
}

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OldFeedConfig {
    /// `!!expr home_path(".jungler/jungler.db")`. MAY BE ABSENT (§14, AGENTS.md).
    pub jungler_db: PathBuf,
    /// `!!expr home_path(".bough/bough.db")`. Opened READ-ONLY, always.
    pub bough_db: PathBuf,
    /// The adapter's OWN watermark store, `!!expr bough_path("old-feed.db")` (P3-D13).
    pub state_db: PathBuf,
    pub poll_ms: u64,
    pub batch: usize,
    /// Which agent receives jungler mail until Phase 5's `mail-router` exists.
    pub deliver_to: String,
    pub priming_limit: usize,
    /// Seal `nodes.summary` / `lane_story` rows as interim tier-1 rollups.
    pub tier1: bool,
}

/// The concrete handle the key's value is.
#[derive(Clone)]
pub struct OldFeedHandle(pub Arc<OldFeedInner>);

/// The adapter's live state: the two source connections, the watermark store, the last sweep.
pub struct OldFeedInner {
    _private: (),
}

impl OldFeedHandle {
    /// §14's cheap win: command memory for PRIMING. Never mail, never a step, never a
    /// projection section in this phase.
    pub async fn prime(&self, _q: &PrimingQuery) -> Result<Vec<CommandMemory>, OldFeedError> {
        todo!("WP-6")
    }

    /// `note_sections` as CITED EVIDENCE: each carries `Cite { ref: "note:<note>#<ord>" }`.
    pub async fn notes(&self, _q: &NoteQuery) -> Result<Vec<NoteEvidence>, OldFeedError> {
        todo!("WP-6")
    }

    /// What the last sweep did. The `/oldfeed` command renders it.
    pub fn status(&self) -> FeedStatus {
        todo!("WP-6")
    }

    /// One sweep: events → mail, `nodes.summary` / `lane_story` → tier-1 rollups, watermarks
    /// advanced last. The poll loop calls it; the tests call it directly.
    pub async fn sweep(&self) -> Result<FeedStatus, OldFeedError> {
        todo!("WP-6")
    }
}

/// The priming filter. Every field is optional; `limit` comes from the config.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrimingQuery {
    pub repo: Option<String>,
    pub tags: Vec<String>,
    pub contains: Option<String>,
    pub limit: usize,
}

/// One remembered command. Competence memory, NEVER mail.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandMemory {
    pub cmd: String,
    pub tags: Vec<String>,
    pub repo: String,
    pub at: DateTime<Utc>,
    pub exit_code: Option<i64>,
    pub output_head: String,
}

/// The notes filter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NoteQuery {
    pub contains: Option<String>,
    pub limit: usize,
}

/// One note section, as cited evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteEvidence {
    pub note: i64,
    pub ord: i64,
    pub heading: String,
    pub body: String,
    pub author: String,
    pub cite: Cite,
}

/// What the last sweep did, per source.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FeedStatus {
    /// `('jungler.events', rows delivered, watermark)` triples.
    pub sources: Vec<(String, usize, i64)>,
    /// Sources that were disabled, and why (absent db, missing required column).
    pub disabled: Vec<(String, String)>,
    pub last_sweep: Option<DateTime<Utc>>,
}

/// Everything the adapter can go wrong as. An ABSENT or unreadable jungler db is NOT one of them.
#[derive(Debug, thiserror::Error)]
pub enum OldFeedError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("ledger: {0}")]
    Ledger(#[from] bough_plugin_ledger::LedgerError),
    #[error("{0}")]
    Failed(String),
}

/// The row.
pub struct OldFeedPlugin;

#[async_trait::async_trait]
impl Plugin for OldFeedPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = OldFeedConfig;

    fn inject() -> Inject {
        Inject::required(["agents", "ledger"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-6: probe, open the state db, provide `old_feed`, spawn the sweep, add /oldfeed")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(OldFeedPlugin);
