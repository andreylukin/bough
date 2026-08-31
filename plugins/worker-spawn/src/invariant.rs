//! No runtime invariant: `worker-spawn` is a PROVIDER of the `workers` seam. Its bounds, its run
//! table and its report/claim steps are all the seam's relations, checked by
//! `bough-plugin-workers`' invariant over exactly this provider's output. What is specific to it
//! — the boundary block, the task-only context — is pinned by
//! `plugins/worker-spawn/tests/roundtrip.rs`. That file's assertion on the recorded `LlmRequest`
//! — the block reaching the ADAPTER, not merely the seed — is `#[ignore]`d until the composition
//! that mounts a loop provider exists (WP-8); the ordering rule itself is pinned purely in the
//! meantime (§0.2).
