//! §0.2: **No runtime invariant.** This row runs foreign hook commands inside the tools
//! waterfalls; it writes no steps and owns no ledger relation or event stream to check. Its rules
//! — discovery order, matcher filtering, verdicts that only tighten — are pure functions pinned
//! by `settings.rs`'s and `outcome.rs`'s unit tests, and monotonicity of the guard itself is the
//! `tools` seam's type-level property, not this row's to re-check.

use bough_kernel::InvariantSpec;

/// No specs: see the module comment.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
