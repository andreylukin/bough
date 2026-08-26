//! Invariant: patch layers are ID-KEYED and there is NO DEEP MERGE, ever (§0.5). `config` is
//! replaced wholesale — if that feels inconvenient, the answer is to restate the fields you keep,
//! not to add a merge. A patch naming an absent row id is a `ComposeWarning`, never an error.

use std::collections::BTreeMap;

use crate::config::entry::{Entry, Inject, RealmLabel};
use crate::config::expr::Expr;
use crate::fiber::EntryId;

/// One layer.
///
/// Document shape (Decision D8): `{ entries: {id: {..}}, insert: [..], remove: [..] }`, with a
/// bare YAML sequence as sugar for inserting those entries at the root end — which is what lets
/// `bundles/bough-base.yml` read as a plain row list.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(from = "PatchRepr")]
pub struct Patch {
    pub entries: BTreeMap<EntryId, EntryPatch>,
    pub insert: Vec<Insert>,
    /// Not in §0.5; here because a profile must be able to drop a base row without knowing what
    /// `disabled` would leave behind (Decision D8).
    pub remove: Vec<EntryId>,
}

impl Default for Patch {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            insert: Vec::new(),
            remove: Vec::new(),
        }
    }
}

impl Patch {
    /// Parse one layer document.
    pub fn parse(yaml: &str) -> Result<Patch, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }
    /// Apply this layer to `rows` in place, recording provenance through `record`.
    ///
    /// The whole algorithm is ~200 lines and has no merge in it (§0.5).
    pub fn apply(&self, rows: &mut Vec<Entry>, record: &mut dyn FnMut(&EntryId, &'static str)) {
        todo!("WP-4")
    }
    /// Row ids this layer names that `rows` does not contain.
    pub fn absent_ids(&self, rows: &[Entry]) -> Vec<EntryId> {
        todo!("WP-4")
    }
}

/// Per-field replacement for one existing row. Every field is `Option`: `None` means "this layer
/// does not write this field", which is what makes provenance per-field.
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryPatch {
    /// REPLACES the whole config. No deep merge (§0.5): restate the fields you keep.
    #[serde(default)]
    pub config: Option<serde_yaml::Value>,
    pub plugin: Option<String>,
    pub disabled: Option<Expr<bool>>,
    pub isolate: Option<BTreeMap<String, RealmLabel>>,
    pub inject: Option<Inject>,
}

/// A new row and where it goes.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Insert {
    #[serde(flatten)]
    pub at: InsertAt,
    pub entry: Entry,
}

/// Insertion position.
#[derive(Clone, Debug, serde::Deserialize)]
pub enum InsertAt {
    Before(EntryId),
    After(EntryId),
    /// Into the named row's `group`.
    Into(EntryId),
    #[serde(other)]
    RootEnd,
}

/// The serde-facing shape of [`Patch`]: the map form, or a bare sequence of entries.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(untagged)]
pub enum PatchRepr {
    Rows(Vec<Entry>),
    Doc {
        #[serde(default)]
        entries: BTreeMap<EntryId, EntryPatch>,
        #[serde(default)]
        insert: Vec<Insert>,
        #[serde(default)]
        remove: Vec<EntryId>,
    },
}

impl From<PatchRepr> for Patch {
    fn from(r: PatchRepr) -> Self {
        todo!("WP-4")
    }
}
