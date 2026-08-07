//! SQLite persistence (port of `src/db/`). The only place in the tree that
//! speaks SQL — **no raw SQL exists outside `db`**. The three ordering rules:
//! `messages_for` orders by `(created_at, rowid)` never `created_at` alone;
//! `thread_for` is ancestors root→parent then own; `ancestor_chain` walks
//! `parent_id` to the lineage root. Timestamps are epoch ms INTEGER, booleans
//! 0/1, structured data JSON TEXT; `PRAGMA foreign_keys = ON` at every open.

pub mod embed;
pub mod extensions;
pub mod migrate;
pub mod sqlite_db;

pub use sqlite_db::{open_db, DbOptions, SqliteDb};
