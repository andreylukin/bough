//! SQLite loadable-extension capability, decided once per process (port of
//! `src/db/extensions.ts`).
//!
//! The whole Homebrew-dylib dance disappears under rusqlite `bundled` (which
//! compiles extension-capable SQLite everywhere); what survives is the
//! `BOUGH_NO_EMBED` env gate and the once-per-process decision. Everything is
//! graceful-absence; nothing here errors.

use std::sync::OnceLock;

static DECISION: OnceLock<bool> = OnceLock::new();

/// Idempotent; the first call decides for the process. `BOUGH_NO_EMBED` set →
/// false; otherwise true (the bundled build can load extensions).
pub fn enable_sqlite_extensions() -> bool {
    *DECISION.get_or_init(|| std::env::var("BOUGH_NO_EMBED").is_err())
}

/// Reports the decision; never triggers it. False when undecided.
pub fn extensions_enabled() -> bool {
    DECISION.get().copied().unwrap_or(false)
}
