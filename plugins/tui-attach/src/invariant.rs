//! §0.2: **No runtime invariant.** This row is a transport: it writes no steps, owns no ledger
//! relation, and its one internal rule — one client at a time, seq-guarded cleanup — is a property
//! of in-process state with no event stream to check it against. It is pinned by the unit tests in
//! `server.rs` (`a_second_register_steals_and_a_stale_end_is_a_no_op`,
//! `close_detaches_and_refuses_later_registrations`) and by the launcher's integration suite.

use bough_kernel::InvariantSpec;

/// No specs: see the module comment.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
