//! §0.2 runtime invariant for `bough-plugin-collector-github`:
//!
//! **No two `mail/delivered` steps on one trajectory carry the same `gh:` ref**, and every step this row appends is EVIDENCE carrying at least one `gh:` ref. The first is the at-least-once ref guard checked against the ledger rather than documented; the second is §3's rule that collected mail is cited by construction.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
