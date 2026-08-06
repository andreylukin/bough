//! Host side of the workflow worker (port of the workflow half of
//! `src/harness/`). Same sidecar architecture as `vm.rs`; determinism traps,
//! stage-major structural coordinates and combinators all stay JS.
//! `WORKFLOW_PROGRAM_PARAMS` is duplicated Rust-side by design; a probe test
//! pins the two lists equal. STUB (wave 3, row 3.8).
