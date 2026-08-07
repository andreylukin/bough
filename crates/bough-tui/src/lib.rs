//! bough-tui — the ratatui client. Speaks ONLY loopback HTTP + SSE.
//!
//! Crate rule (ARCHITECTURE.md §1): this crate may use only
//! `bough_core::{schema, errors, types::Effort/UsageTotals}` — it is a wire
//! client, not a domain participant. It must never link the Db or LLM paths.
//! No URL string outside `api`. The reducer stays single-threaded and pure:
//! SSE reader, timers and input all post actions over one mpsc.

// `Action`, `StoreAction` and `ForestRow` are the enums every event and every
// rendered row passes through. Boxing the wide variants to even the sizes would
// put an allocation on the per-keystroke and per-SSE-frame path to save stack on
// an enum that is moved once and matched immediately.
#![allow(clippy::large_enum_variant)]
// Timer entries and the render seams hold `Box<dyn Fn()>` tuples; naming them
// through aliases hides the shape at the one place it needs to be legible
// (matching bough-core's rule for its injection seams).
#![allow(clippy::type_complexity)]

pub mod ansi;
pub mod api;
pub mod app;
pub mod args;
pub mod clipboard;
pub mod components;
pub mod events;
pub mod forest;
pub mod format;
pub mod input;
pub mod keys;
pub mod lines;
pub mod paste;
pub mod selection;
pub mod store;
pub mod term;
pub mod theme;

/// Run the TUI against the loopback server: preflight first (a dead server is
/// a sentence and exit 2, never a blank screen — main.tsx contract), then the
/// event loop over one mpsc of actions (`app::run_live`). The returned error
/// string is user-facing; the bin prints it and exits 2.
pub fn run(options: app::TuiOptions) -> Result<(), bough_core::errors::BoughError> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| bough_core::errors::BoughError::bad_request(format!("bough tui: {e}")))?;
    rt.block_on(app::run_live(options))
        .map_err(bough_core::errors::BoughError::bad_request)
}
