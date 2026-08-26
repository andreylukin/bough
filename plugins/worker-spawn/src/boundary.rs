//! Invariant (§7, P2-D21): the write boundary is a SECURITY INVARIANT, so it lives in code as a
//! `const` and not in config. A patch can disable the row — that is Andrey's act — and cannot
//! edit this text.

/// The standing block the SPAWNER prepends to every worker's task (§10). WP-6 writes the prose;
/// its position (first, always) is the part that is normative.
pub const WRITE_BOUNDARY: &str = "WP-6: the standing write-boundary block (§7). \
Prepended by the spawner to every worker task, before the task itself.";
