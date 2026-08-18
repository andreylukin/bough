//! bough-core — everything that is not HTTP and not a terminal.
//!
//! Crate rules (architecture.md §1):
//! - `hostfn` / `turn` / `history` / `agents` never reference `bough-server`;
//!   they return `BoughError` and only the server crate converts errors to
//!   responses.
//! - No raw SQL outside `db`. No provider name outside `llm`.
//! - Every JSON wire shape and every DB row type is defined ONCE, in `schema`
//!   (plus the port types in `types`), serde-derived, field names matching the
//!   TS wire format exactly.

// The injection seams this crate is built on (architecture.md §1: the ports in
// `types`, and the `Deps`/`Opts` structs each module takes) are `Arc<dyn Fn(..)>`
// fields standing in for what TS passed as a plain function argument. Naming each
// one through a type alias would add a layer of indirection over the exact place
// the shape is supposed to be legible.
#![allow(clippy::type_complexity)]
// The module headers carry hand-aligned definition lists (`fork` mode tables,
// `delegate` tiers) whose continuation lines line up under the term they
// describe. Rustdoc's preferred 4-space indent would break that alignment in the
// source, which is where these are actually read.
#![allow(clippy::doc_overindented_list_items)]
#![allow(clippy::doc_lazy_continuation)]

pub mod agents;
pub mod bus;
pub mod config;
pub mod db;
pub mod errors;
pub mod extensions;
pub mod harness;
pub mod history;
pub mod hooks;
pub mod hostfn;
pub mod llm;
pub mod logs;
pub mod mcp;
pub mod notes;
pub mod paths;
pub mod plugins;
pub mod prompt;
pub mod resume;
pub mod schedules;
pub mod schema;
pub mod scratch;
pub mod skills;
pub mod switches;
pub mod turn;
pub mod types;
pub mod vcs;
pub mod worker;
pub mod workflow;
