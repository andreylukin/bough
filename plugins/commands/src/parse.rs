//! Invariant: parsing is PURE and total. It never resolves, never dispatches and never reads a
//! registry; a line either is a command line or is text, and that verdict depends on the line and
//! the prefix alone.

use crate::{CommandName, Invocation};

/// PURE. `None` when the line does not start with the prefix; a doubled prefix (`//x`) is
/// literal text and yields `None`, so a message can begin with a slash.
pub fn parse(_line: &str, _prefix: char) -> Option<Invocation> {
    todo!("WP-1: prefix rule, shell-style split with quoted runs kept whole")
}

/// Shell-style split of a command's argument tail: quoted runs stay whole.
pub fn split_args(_tail: &str) -> Vec<String> {
    todo!("WP-1")
}

/// The nearest registered name to an unknown one, by edit distance, or `None` when nothing is
/// close enough to be a suggestion rather than a guess.
pub fn did_you_mean(_name: &str, _known: &[CommandName]) -> Option<String> {
    todo!("WP-1")
}
