//! Invariant: a filter word this crate does not understand is an ERROR naming the word, never a
//! silently dropped conjunct (§16). A filter that quietly matched more than it said would make
//! every row on screen a claim nobody can check.

use bough_plugin_ledger::LedgerError;

/// What `parse_filter` refuses.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum FilterError {
    #[error("`{0}` is not a filter; try agent:/ref:/type:/class:/since:/until:")]
    UnknownWord(String),
    #[error("`{word}`: {detail}")]
    BadValue { word: String, detail: String },
    #[error("since {since} is after until {until}")]
    EmptyWindow { since: String, until: String },
}

/// What the pane's one read can fail with.
#[derive(Debug, thiserror::Error)]
pub enum TimelineError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Filter(#[from] FilterError),
}
