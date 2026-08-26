//! Invariant (§17): Phase 5 curates the cross-agent timeline's DATA; the PANE is Phase 8. A
//! `timeline/entry` is Evidence and carries cites, because a timeline is rendered as truth.

use bough_plugin_ledger::{AgentName, Cite, Ref};
use chrono::{DateTime, Utc};

/// One entry the leader notes.
#[derive(Clone, Debug)]
pub struct TimelineEntry {
    pub title: String,
    /// The moment the entry is ABOUT, which is not the moment it was written.
    pub at: DateTime<Utc>,
    pub agents: Vec<AgentName>,
    pub refs: Vec<Ref>,
    pub cites: Vec<Cite>,
}

/// Which entries to read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimelineQuery {
    pub agent: Option<AgentName>,
    pub since: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
}

/// One entry as it was stored.
#[derive(Clone, Debug, PartialEq)]
pub struct TimelineRow {
    pub step: bough_plugin_ledger::StepId,
    pub title: String,
    pub at: DateTime<Utc>,
    pub agents: Vec<AgentName>,
    pub refs: Vec<Ref>,
}
