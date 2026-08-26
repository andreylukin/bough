//! Invariant: ONE composition path. `--dump-config` and boot both call [`compose_for`], and the
//! dump is `render()` of exactly the `Composition` that boot hands the kernel. That identity is
//! the whole point of V6 — a second pretty-printer or a second layer stack is how a dump starts
//! lying about what booted.
//!
//! Layer order, normative (§0.5), and the order the `LayerId`s appear in `Composition::layers`:
//!
//! ```text
//! empty root
//!   → bundles/<b>.yml for each b in profile.bundles, in the profile's order  "bundle:<b>"
//!   → the profile's own `patch:` block                                       "profile:<name>"
//!   → ~/.bough/bough.patch.yml (absent ⇒ skipped silently)                   "user"
//!   → each --patch FILE, in argument order                                   "patch:<n>:<file>"
//! ```

use bough_kernel::{Catalog, Composition};

use crate::cli::{BootError, Cli};

/// Stack every layer and produce the composition.
pub fn compose_for(cli: &Cli, catalog: &Catalog) -> Result<Composition, BootError> {
    todo!("WP-5")
}
