//! No runtime invariant: `projection-probe` is a TEST INSTRUMENT (§0.2 permits a stated reason in
//! place of a check). It owns no data relation and no event stream of its own — everything it does
//! is asserted directly by the swap and invariant tests that mount it, and the relations it
//! touches are policed by the ledger's and the projection's own invariant modules. Phase 8's
//! fixture audit removes this crate.

use bough_kernel::InvariantSpec;

/// None, deliberately. See the module statement.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
