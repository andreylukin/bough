//! The wire contract. Every shape that crosses server↔client, server↔db, or
//! server↔worker is declared here once, serde-derived, so there is exactly one
//! definition of "what a Message is" for the router, the TUI store, the CLI and
//! the database layer to agree on. camelCase on the wire; snake_case in storage;
//! the row→domain mappers in `db::sqlite_db` are the ONLY translation point.

pub mod events;
pub mod parts;
pub mod requests;

pub use events::*;
pub use parts::*;
pub use requests::*;
