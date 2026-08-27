//! Invariant (decision D-C7): the scan is LEXICAL and complete over the roots it is given. A regex
//! scanner would miss exactly the cases that matter — a `const MODE` override that disagrees with
//! its trait, one `NAME` under two modes, a type impl'ing two event traits dispatched under the
//! wrong one — so this parses with `syn` and a file that does not parse is an ERROR naming the
//! file, never a silently skipped file (§16).

use std::path::{Path, PathBuf};

/// The four dispatch modes the kernel's event traits spell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DispatchMode {
    Emit,
    Parallel,
    Serial,
    Waterfall,
}

impl DispatchMode {
    /// The mode an event trait's path names, when it is one of the four.
    ///
    /// WP-6.
    pub fn from_trait(path: &str) -> Option<DispatchMode> {
        let _ = path;
        todo!("WP-6: EmitEvent/ParallelEvent/SerialEvent/WaterfallEvent")
    }

    /// The mode a dispatch method name names.
    ///
    /// WP-6.
    pub fn from_method(name: &str) -> Option<DispatchMode> {
        let _ = name;
        todo!("WP-6: emit/parallel/serial/waterfall")
    }
}

/// One declared event: an `impl <EventTrait> for <Ty>` with its `const NAME`.
#[derive(Clone, Debug, PartialEq)]
pub struct EventDecl {
    /// The `const NAME` literal.
    pub name: String,
    /// The impl's `Self` type.
    pub ty: String,
    /// From the TRAIT: Emit / Parallel / Serial / Waterfall.
    pub trait_mode: DispatchMode,
    /// An explicit `const MODE = …`, when the impl carries one.
    pub declared_mode: Option<DispatchMode>,
    pub krate: String,
    pub file: PathBuf,
    pub line: usize,
}

/// Whether a site DISPATCHES an event or LISTENS for one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SiteKind {
    Dispatch,
    Listen,
}

/// One call site: `ctx.emit::<X>(…)`, `ctx.waterfall::<X>(…)`, `ctx.on::<X>(…)`.
#[derive(Clone, Debug, PartialEq)]
pub struct DispatchSite {
    pub ty: String,
    pub mode: DispatchMode,
    pub kind: SiteKind,
    pub file: PathBuf,
    pub line: usize,
}

/// Everything one scan found.
#[derive(Clone, Debug, PartialEq)]
pub struct Catalog {
    pub decls: Vec<EventDecl>,
    pub sites: Vec<DispatchSite>,
}

/// Why a scan could not finish.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0} does not parse as Rust: {1}")]
    Parse(PathBuf, syn::Error),
}

/// Parse every `.rs` under `roots` with `syn` and collect declarations and sites.
///
/// WP-6.
pub fn scan(roots: &[&Path]) -> Result<Catalog, ScanError> {
    let _ = roots;
    todo!("WP-6: walk the roots, parse each file, visit ItemImpl and every turbofish call")
}
