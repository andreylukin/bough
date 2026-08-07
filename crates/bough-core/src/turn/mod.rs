//! The turn state machine (port of `src/turn/`). One turn per session; the
//! drive loop is a single sequential async fn — tools execute one at a time by
//! design. `begin` claims synchronously; the epilogue (registry release →
//! drain via `has_unanswered_input`) runs on every path including panics.

pub mod queue;
pub mod replay;
pub mod runner;
pub mod state;

#[cfg(test)]
pub(crate) mod testkit;
