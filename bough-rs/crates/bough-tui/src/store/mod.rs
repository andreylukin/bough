//! The TUI store (port of `src/tui/store/`). The reducer is single-threaded
//! and pure — SSE reader, timers and input all post actions over one mpsc.
//! Event handling matches the closed `EventType` enum exhaustively with NO
//! default arm: a new event type must be a compile error.

pub mod reduce;
pub mod selectors;
pub mod shell;
pub mod state;
