//! Invariant: an entry is the whole unit of composition — a row has an id, at most one plugin, a
//! config, a disabled expression, a realm map, an inject set and children. `deny_unknown_fields`
//! is deliberate: a typo in a bundle must be loud rather than silently ignored (§0.2).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::config::expr::Expr;
use crate::fiber::EntryId;

bough_util::brand_id!(
    /// An `isolate:` realm label. Entries sharing a label share the binding for that key.
    pub struct RealmLabel;
);

/// One row of the config tree.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: EntryId,
    /// `None` ⇒ a pure group row: it owns children, a realm map and an inject set, and is ACTIVE
    /// as soon as it is mounted (Decision D18).
    #[serde(default)]
    pub plugin: Option<String>,
    /// `Null` when absent. May contain `!!expr` nodes, evaluated at mount.
    #[serde(default)]
    pub config: serde_yaml::Value,
    /// Literal bool or `!!expr`.
    #[serde(default)]
    pub disabled: Expr<bool>,
    /// service `NAME` → realm.
    #[serde(default)]
    pub isolate: BTreeMap<String, RealmLabel>,
    #[serde(default)]
    pub inject: Inject,
    /// Children; effects of this row's fiber, so unloading this row cascades.
    #[serde(default)]
    pub group: Vec<Entry>,
    /// Grafted at parse time, BEFORE any patch layer, so a later layer can patch an included row
    /// by id (Decision D19).
    #[serde(default)]
    pub include: Option<PathBuf>,
}

/// A row's declared service dependencies.
///
/// YAML shape (Decision D2): `inject: [a, b]` means both required;
/// `inject: {required: [a], optional: [b]}` is the long form.
#[derive(
    Clone, Default, Debug, PartialEq, serde::Deserialize, serde::Serialize, schemars::JsonSchema,
)]
#[serde(from = "InjectRepr", into = "InjectRepr")]
pub struct Inject {
    pub required: BTreeSet<String>,
    pub optional: BTreeSet<String>,
}

impl Inject {
    /// Declares nothing.
    pub fn none() -> Self {
        Self::default()
    }
    /// All keys required.
    pub fn required(keys: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            required: keys.into_iter().map(str::to_owned).collect(),
            optional: BTreeSet::new(),
        }
    }
    /// All keys optional.
    pub fn optional(keys: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            required: BTreeSet::new(),
            optional: keys.into_iter().map(str::to_owned).collect(),
        }
    }
    /// Entry ∪ plugin-static (Decision D1). The entry may ADD keys; it may not drop a plugin's
    /// static requirement, so a key required by either side stays required.
    pub fn union(&self, other: &Inject) -> Inject {
        todo!("WP-4")
    }
    /// Whether `name` appears in either set.
    pub fn declares(&self, name: &str) -> bool {
        self.required.contains(name) || self.optional.contains(name)
    }
}

/// The serde-facing shape of [`Inject`]: a bare list, or the long map form.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum InjectRepr {
    List(Vec<String>),
    Map {
        #[serde(default)]
        required: Vec<String>,
        #[serde(default)]
        optional: Vec<String>,
    },
}

impl From<InjectRepr> for Inject {
    fn from(r: InjectRepr) -> Self {
        todo!("WP-4")
    }
}

impl From<Inject> for InjectRepr {
    fn from(i: Inject) -> Self {
        todo!("WP-4")
    }
}

/// Parse a YAML document into rows, grafting every `include:` in place.
///
/// `base` is the directory relative includes resolve against. An include cycle is an error naming
/// the path (§0.5).
pub fn parse_entries(
    yaml: &str,
    base: &std::path::Path,
) -> Result<Vec<Entry>, crate::error::ComposeError> {
    todo!("WP-4")
}
