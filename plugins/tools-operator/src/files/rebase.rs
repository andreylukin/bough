//! Invariant: a rebase re-checks the ACTUAL lines rather than trusting a tag. An untouched range
//! moves; a touched one conflicts, naming the line range, rather than being applied blind.

use super::grammar::{PatchOp, PatchError};

/// A map from each line of `base` to its line in `cur`, or `None` if it is gone.
///
/// WP-3 owns the body.
pub fn line_map(_base: &[String], _cur: &[String]) -> Vec<Option<usize>> {
    todo!("WP-3: port main's line_map")
}

/// What rebasing a file's operations onto a moved file produced.
#[derive(Clone, Debug, PartialEq)]
pub enum RebaseResult {
    /// The file did not move.
    Unchanged,
    /// The operations were shifted onto the current coordinates.
    Rebased(Vec<PatchOp>),
    Conflict(RebaseConflict),
}

/// The conflict a rebase reports, in the coordinates the model wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct RebaseConflict {
    pub path: String,
    pub from: usize,
    pub to: usize,
    pub detail: String,
}

impl From<RebaseConflict> for PatchError {
    fn from(c: RebaseConflict) -> PatchError {
        PatchError::Conflict {
            path: c.path,
            from: c.from,
            to: c.to,
        }
    }
}

/// Rebase `ops` from `base`'s coordinates onto `cur`'s.
///
/// WP-3 owns the body.
pub fn rebase_ops(_ops: &[PatchOp], _base: &[String], _cur: &[String]) -> RebaseResult {
    todo!("WP-3: port main's rebase_ops")
}
