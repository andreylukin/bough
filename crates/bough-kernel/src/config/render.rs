//! Invariant: ONE renderer (Decision D9). `--dump-config` prints `render(&composition)` and the V6
//! test prints `render(&kernel.composition())`; there is no second formatter, because a second one
//! is how a dump starts lying about what booted.

use crate::config::compose::Composition;

/// Output shape for `--dump-config`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DumpFormat {
    Yaml,
    Json,
}

/// Render a composition: every row annotated with the layer that last wrote each field, each
/// `!!expr` shown as both its raw source and its resolved value, and the fingerprint.
///
/// Deterministic: the same `Composition` always renders byte-identically.
pub fn render(c: &Composition, format: DumpFormat) -> String {
    todo!("WP-4")
}
