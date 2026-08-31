//! §0.2: **No runtime invariant.** This row contributes one projection section rendered from
//! files at request time; it writes no steps and owns no ledger relation or event stream to check.
//! Its rules — duplicate content injected once, order-faithful selection, fence-atomic blocks —
//! are pure functions pinned by `dedup.rs`'s and `lib.rs`'s unit tests.

use bough_kernel::InvariantSpec;

/// No specs: see the module comment.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
