//! Host functions (port of `src/hostfn/`). Host functions take a
//! `types::TurnCtx` and nothing else — this module must never reference the
//! server crate; it returns `BoughError` and only the server converts errors
//! to responses. Error text here is model-facing product surface: every
//! message is ported verbatim.

pub mod artifact;
pub mod ask;
pub mod delegate;
pub mod files;
pub mod jobs;
pub mod patch;
pub mod schedule;
pub mod shell;
pub mod spill;
pub mod state;
