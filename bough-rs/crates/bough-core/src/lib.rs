//! bough-core — everything that is not HTTP and not a terminal.
//!
//! Crate rules (ARCHITECTURE.md §1):
//! - `hostfn` / `turn` / `history` / `agents` never reference `bough-server`;
//!   they return `BoughError` and only the server crate converts errors to
//!   responses.
//! - No raw SQL outside `db`. No provider name outside `llm`.
//! - Every JSON wire shape and every DB row type is defined ONCE, in `schema`
//!   (plus the port types in `types`), serde-derived, field names matching the
//!   TS wire format exactly.

pub mod agents;
pub mod bus;
pub mod db;
pub mod errors;
pub mod harness;
pub mod history;
pub mod hostfn;
pub mod llm;
pub mod mcp;
pub mod paths;
pub mod prompt;
pub mod schedules;
pub mod schema;
pub mod scratch;
pub mod skills;
pub mod turn;
pub mod types;
pub mod vcs;
pub mod worker;
pub mod workflow;
