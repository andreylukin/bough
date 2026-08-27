//! Invariant (§15 item 7): every kernel event is listed with its declared dispatch mode, and a
//! dispatch site that uses a different mode than its type declares FAILS the gate. The four event
//! traits already make the mode compile-checked; what is left is lexical (a `const MODE` override,
//! one `NAME` under two modes, a type impl'ing two traits dispatched under the wrong one), so the
//! gate is a `syn` scan of the source (decision D-C7).
//!
//! `xtask` is a build tool: it is not shipped, holds no `ctx` key, and links nothing from the tree.
//!
//! SCAFFOLD: `allow(unused_variables)` covers the `todo!()` bodies and comes out with them.
#![allow(unused_variables)]

pub mod check;
pub mod scan;
pub mod table;

pub use crate::check::{check, Finding};
pub use crate::scan::{scan, Catalog, DispatchMode, DispatchSite, EventDecl, ScanError, SiteKind};
pub use crate::table::{table, CATALOG_FLOOR};

/// The roots the gate scans, relative to the workspace root.
pub const ROOTS: [&str; 2] = ["crates", "plugins"];

/// The path the committed catalog lives at.
pub const CATALOG_PATH: &str = "docs/event-catalog.md";
