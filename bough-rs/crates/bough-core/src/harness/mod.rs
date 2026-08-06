//! Code-mode workers (port of `src/harness/`). The two worker scripts stay
//! JavaScript and run in a sidecar JS runtime process (Bun if on PATH, else
//! Node ≥ 20), speaking the existing worker protocol as NDJSON over
//! stdin/stdout; only the host side is Rust. Nothing here is a security
//! boundary. THE INVARIANT: **a program never outlives its turn, and never
//! takes the server with it.**

pub mod preflight;
pub mod protocol;
pub mod vm;
pub mod wf;
