//! No runtime invariant: `worker-spawn` is a PROVIDER of the `workers` seam. Its bounds, its run
//! table and its report/claim steps are all the seam's relations, checked by
//! `bough-plugin-workers`' invariant over exactly this provider's output. What is specific to it
//! — the boundary block, the task-only context — is pinned by
//! `plugins/worker-spawn/tests/roundtrip.rs`, which asserts on the recorded `LlmRequest` rather
//! than on the prose that asked for it (§0.2).
