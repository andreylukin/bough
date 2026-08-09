//! The system prompt (port of `src/prompt/`). `system` is the STABLE prefix —
//! byte-identical across sessions per delegation tier (prompt-cache contract);
//! per-session facts belong in `systemVolatile`. Section files are
//! `include_str!`-ed; a missing section file is fatal at boot. AGENTS.md
//! (global `~/.bough` + workspace root — NEVER CLAUDE.md) is re-read per turn.

pub mod assemble;
pub mod last;
pub mod project;
