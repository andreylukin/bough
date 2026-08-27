//! §0.2 statement for `bough-plugin-fault-inject`:
//!
//! **No runtime invariant: the row exists to violate things on purpose; an invariant over it would
//! assert its own faults.** What must hold about it — that it fires on the hit it says it will,
//! that `times: 1` fires exactly once, that an agent filter leaves other agents alone — is pure
//! and is pinned by `sites::tests`.

use bough_kernel::InvariantSpec;

/// No specs, by the statement above.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
