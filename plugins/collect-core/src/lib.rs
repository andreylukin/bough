//! Invariant: delivery is AT-LEAST-ONCE WITH A REF GUARD. The guard runs BEFORE the delivery and
//! the watermark is written AFTER it — that ordering is the whole of the argument, and it is why a
//! restart re-sweep duplicates nothing even when the watermark write was the thing that was lost.
//!
//! This crate has NO ROW (§0.2's one-crate-one-row rule is about rows, not libraries): it exists so
//! `collector-github` and `collector-linear` cannot drift on the part that must not drift.
//!
//! P6-D15: the guard is per (trajectory, ref). Two agents both configured for one repo each get
//! their own copy; deduping globally would silently starve the second.

pub mod delivery;
pub mod guard;
pub mod state;

use std::collections::BTreeSet;

use bough_plugin_agents::MailClass;
use bough_plugin_ledger::Ref;
use chrono::{DateTime, Utc};

pub use delivery::delivery_of;
pub use guard::already_delivered;
pub use state::{Watermark, WatermarkStore};

/// The collector-neutral shape both collectors produce, so [`delivery_of`] and the dedupe guard
/// are written once.
#[derive(Clone, Debug, PartialEq)]
pub struct Collected {
    /// The item's own ref: `gh:o/r#12`, `linear:TEAM-123`. What it is cited BY.
    pub r#ref: Ref,
    pub url: Option<String>,
    pub subject: String,
    pub summary: String,
    pub text: String,
    /// Extra refs the item mentions: `gh:o/r#12`, `linear:TEAM-123`, `lane:…`. What Phase 5's
    /// mail-router will route on.
    pub refs: BTreeSet<Ref>,
    pub class: MailClass,
    pub at: DateTime<Utc>,
    /// Ordering key for the watermark (a numeric id, or a timestamp in millis).
    pub order: i64,
}

/// Which collected classes are wake-class for this row.
///
/// P6-D6: §5 puts the wake-class set per AGENT; there is no per-agent policy surface on this
/// branch, so it lives on the collector until Phase 5's `mail-router` takes it.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WakeClass {
    ReviewRequest,
    Mention,
    Ask,
    Assigned,
}

/// What one sweep did, per source. Rendered by a `/collect` command and asserted on by the tests.
#[derive(Clone, Debug, PartialEq)]
pub struct SweepReport {
    pub collector: &'static str,
    /// `(source, delivered, skipped_as_duplicate, watermark)`.
    pub sources: Vec<(String, usize, usize, i64)>,
    /// Every source that is off, and why. Reported EVERY sweep, never silently skipped (§0.2).
    pub disabled: Vec<(String, String)>,
    pub last_sweep: Option<DateTime<Utc>>,
}

/// What a collector refuses.
#[derive(Debug, thiserror::Error)]
pub enum CollectError {
    #[error("watermark store: {0}")]
    State(String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("`{0}` is not a live agent, so its mail has nowhere to go")]
    NoSuchAgent(String),
    #[error(transparent)]
    Ledger(#[from] bough_plugin_ledger::LedgerError),
    #[error(transparent)]
    Agent(#[from] bough_plugin_agents::AgentError),
}

/// The ref scheme (P6-D5), in one place so two collectors and a router cannot spell it two ways.
pub mod refs {
    use bough_plugin_ledger::Ref;

    /// `gh:o/r#12`. Qualified by repo: the shorter `gh:pr:<n>` is not unique across repos.
    pub fn pr(repo: &str, number: u64) -> Ref {
        Ref::new(format!("gh:{repo}#{number}"))
    }
    /// `gh:o/r#12:thread:<id>`.
    pub fn thread(repo: &str, number: u64, id: &str) -> Ref {
        Ref::new(format!("gh:{repo}#{number}:thread:{id}"))
    }
    /// `gh:o/r#12:comment:<id>`.
    pub fn comment(repo: &str, number: u64, id: &str) -> Ref {
        Ref::new(format!("gh:{repo}#{number}:comment:{id}"))
    }
    /// `gh:o/r#12:check:<name>`.
    pub fn check(repo: &str, number: u64, name: &str) -> Ref {
        Ref::new(format!("gh:{repo}#{number}:check:{name}"))
    }
    /// `linear:TEAM-123`.
    pub fn issue(key: &str) -> Ref {
        Ref::new(format!("linear:{key}"))
    }
    /// `linear:TEAM-123:comment:<id>`.
    pub fn issue_comment(key: &str, id: &str) -> Ref {
        Ref::new(format!("linear:{key}:comment:{id}"))
    }
}
