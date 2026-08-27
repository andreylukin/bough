//! §0.2 runtime invariant for `bough-plugin-tui-status`:
//!
//! **The status line is exactly one terminal row, and every number on it comes from a ledgered
//! fact.** Nothing on the line wraps, nothing overflows its width, and a value the ledger has not
//! recorded (an unknown model price, a projection with no header yet) renders as `—` rather than
//! as a plausible zero — the line is the most-read chrome in the product, so a fabricated number
//! there is the most expensive lie the surface can tell (§16, phase ux1 §2.5).

use bough_kernel::{Cadence, InvariantSpec};

/// The specs this row contributes. WP-4 fills in the recorder and the check.
pub fn specs() -> Vec<InvariantSpec> {
    let _ = Cadence::OnQuiesce;
    todo!("WP-4: record the last rendered line; check its height and its `—` for unknowns")
}
