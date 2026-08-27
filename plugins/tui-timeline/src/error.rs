//! Invariant (§16): an unknown filter word is an ERROR naming the word, never a silently ignored
//! filter. A timeline that quietly drops a conjunct shows more rows than the human asked for and
//! says nothing about it.

/// Why a filter string could not be parsed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum FilterError {
    #[error("`{0}` is not a filter; try agent:/ref:/type:/class:/since:/until:")]
    UnknownWord(String),
    #[error("`{word}`: {detail}")]
    BadValue { word: String, detail: String },
    #[error("since {since} is after until {until}")]
    EmptyWindow { since: String, until: String },
}
