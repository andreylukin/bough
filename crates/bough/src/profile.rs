//! Invariant: an installed binary must boot with no repo checked out. Profiles and bundles are
//! embedded with `include_dir!` and overridable, in order, by `--root DIR` and then
//! `$BOUGH_HOME/{profiles,bundles}` (Decision D11).
//!
//! Known trap from the old tree: `include_dir`'s `files()` is NOT recursive — use `find()` or
//! `get_file()` with explicit paths.

use std::path::{Path, PathBuf};

use bough_kernel::Patch;

use crate::cli::BootError;

/// The embedded copies. Present so `bough` works from an installed binary.
pub static EMBEDDED_PROFILES: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../profiles");
/// See [`EMBEDDED_PROFILES`].
pub static EMBEDDED_BUNDLES: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../bundles");

/// One profile document.
#[derive(Debug, serde::Deserialize)]
pub struct Profile {
    pub name: String,
    /// Bundles to stack, in this order.
    pub bundles: Vec<String>,
    /// Create the kernel's invariant runner (`dev` only in Phase 0).
    #[serde(default)]
    pub invariants: bool,
    /// The profile's own patch layer, stacked after every bundle.
    #[serde(default)]
    pub patch: Patch,
}

/// Where each document actually came from, so an error can name the search path and
/// `--dump-config` can be honest about provenance.
#[derive(Debug, Clone)]
pub struct Sources {
    /// Directories searched, in order.
    pub searched: Vec<PathBuf>,
    /// Resolved profile document.
    pub profile: SourceOrigin,
    /// Resolved bundle documents, in profile order.
    pub bundles: Vec<(String, SourceOrigin)>,
}

/// Where one document was read from.
#[derive(Debug, Clone)]
pub enum SourceOrigin {
    File(PathBuf),
    Embedded(&'static str),
}

/// Resolve a profile and the bundles it names.
pub fn resolve_profile(name: &str, root: Option<&Path>) -> Result<(Profile, Sources), BootError> {
    todo!("WP-5")
}

/// Read one bundle document as a patch layer.
pub fn load_bundle(
    name: &str,
    root: Option<&Path>,
) -> Result<(Patch, SourceOrigin), BootError> {
    todo!("WP-5")
}
