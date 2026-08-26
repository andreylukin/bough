//! Invariant (§4's undo rules): an UNUSED split undoes as POINTERS — delete the child rows,
//! restore the parent's refs from the op step, append `graph/undo`, and call no model at all. A
//! LIVED-IN one undoes as a MERGE with the parent as survivor, which writes the reconciliation
//! digest and leaves the divergent heads behind by construction, because no trajectory is ever
//! deleted.

//! The body is WP-3's; the entry point is called from [`crate::GraphHandle`].
