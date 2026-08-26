//! Invariant: composition is the ONE path from layers to a live tree — `--dump-config` and boot
//! call the same function over the same `Composition`, which is the whole point of V6. The
//! fingerprint is computed on the EVALUATED tree (Decision D9), so an `!!expr` result change moves
//! it; a comment or a key reordering does not.

use std::collections::BTreeMap;

use crate::catalog::Catalog;
use crate::config::entry::Entry;
use crate::config::expr::ExprEnv;
use crate::config::patch::Patch;
use crate::config::{ComposeWarning, LayerId};
use crate::error::ComposeError;
use crate::fiber::EntryId;

/// sha256, hex, over the canonical JSON of the evaluated tree.
///
/// For every row, in tree order: `id`, `plugin`, `config` (map keys sorted), `disabled` (resolved
/// bool), `isolate`, `inject`, then `group` recursively. Provenance, warnings and layer ids are
/// excluded. This is the COMPOSITION FINGERPRINT later phases put on requests and headers.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Fingerprint(String);

impl Fingerprint {
    /// Hash an evaluated tree.
    pub fn of(tree: &[Entry]) -> Fingerprint {
        todo!("WP-4")
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The result of stacking every layer: what boots, and everything needed to explain it.
#[derive(Clone, Debug)]
pub struct Composition {
    /// After `!!expr` evaluation. This is what the kernel mounts.
    pub tree: Vec<Entry>,
    /// Before evaluation, so the dump can print both the raw expression and its resolved value.
    pub raw: Vec<Entry>,
    pub provenance: BTreeMap<EntryId, RowProvenance>,
    pub layers: Vec<LayerId>,
    pub warnings: Vec<ComposeWarning>,
    pub fingerprint: Fingerprint,
}

/// Which layer created a row, and which layer last wrote each of its fields.
#[derive(Clone, Debug)]
pub struct RowProvenance {
    pub created_by: LayerId,
    pub fields: BTreeMap<&'static str, LayerId>,
}

/// Stacks layers in order over an empty root.
pub struct Composer {
    _priv: (),
}

impl Composer {
    /// Bind a catalog (for schema validation) and an expression environment.
    pub fn new(catalog: &Catalog, env: ExprEnv) -> Self {
        todo!("WP-4")
    }
    /// Push one layer. Order is normative (§0.5): bundles, profile patch, user patch, `--patch`.
    pub fn layer(&mut self, id: LayerId, patch: Patch) -> &mut Self {
        todo!("WP-4")
    }
    /// Apply layers in order, evaluate `!!expr`, then validate EVERY row: an unknown plugin name
    /// is an error, a config the plugin's schema or `validate` rejects is an error naming the row,
    /// and the first bad row rejects the whole candidate. A patch naming an absent row id is a
    /// WARNING (§0.2).
    pub fn compose(self) -> Result<Composition, ComposeError> {
        todo!("WP-4")
    }
}
