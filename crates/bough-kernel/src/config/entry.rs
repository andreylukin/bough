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
    /// Required in Phase 0. Decision D18 (`None` ⇒ a pure group row, ACTIVE as soon as it is
    /// mounted) is NOT implemented: the composer rejects a row that names no plugin, so that
    /// `--dump-config` and the mount path agree. A group is expressed by giving the parent row a
    /// plugin of its own.
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
        let mut required = self.required.clone();
        required.extend(other.required.iter().cloned());
        let mut optional = self.optional.clone();
        optional.extend(other.optional.iter().cloned());
        // A key required by either side stays required, so `optional` never shadows `required`.
        optional.retain(|k| !required.contains(k));
        Inject { required, optional }
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
        match r {
            InjectRepr::List(keys) => Inject {
                required: keys.into_iter().collect(),
                optional: BTreeSet::new(),
            },
            InjectRepr::Map { required, optional } => {
                let required: BTreeSet<String> = required.into_iter().collect();
                let optional: BTreeSet<String> = optional
                    .into_iter()
                    .filter(|k| !required.contains(k))
                    .collect();
                Inject { required, optional }
            }
        }
    }
}

impl From<Inject> for InjectRepr {
    fn from(i: Inject) -> Self {
        // Always the long form on the way out: the list form cannot express `optional`, and a
        // shape that changes with the data would make `--dump-config` inconsistent row to row.
        InjectRepr::Map {
            required: i.required.into_iter().collect(),
            optional: i.optional.into_iter().collect(),
        }
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
    let layer = crate::config::LayerId::new("<entries>");
    let normalized = crate::config::expr::normalize_expr_tags(yaml);
    let mut rows: Vec<Entry> =
        serde_yaml::from_str(&normalized).map_err(|e| crate::error::ComposeError::BadYaml {
            layer: layer.clone(),
            detail: e.to_string(),
        })?;
    let mut stack: Vec<PathBuf> = Vec::new();
    graft_includes(&mut rows, base, &mut stack, &layer)?;
    Ok(rows)
}

/// Depth-first graft of every `include:` in `rows`. `stack` is the chain of files currently being
/// grafted, so a cycle is caught by identity rather than by a depth counter.
pub(crate) fn graft_includes(
    rows: &mut [Entry],
    base: &std::path::Path,
    stack: &mut Vec<PathBuf>,
    layer: &crate::config::LayerId,
) -> Result<(), crate::error::ComposeError> {
    for row in rows.iter_mut() {
        if let Some(rel) = row.include.take() {
            let path = if rel.is_absolute() {
                rel.clone()
            } else {
                base.join(&rel)
            };
            let key = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if stack.contains(&key) {
                return Err(crate::error::ComposeError::BadInclude {
                    path: path.clone(),
                    layer: layer.clone(),
                    detail: format!(
                        "include cycle: {} is already being included",
                        path.display()
                    ),
                });
            }
            let text = std::fs::read_to_string(&path).map_err(|e| {
                crate::error::ComposeError::BadInclude {
                    path: path.clone(),
                    layer: layer.clone(),
                    detail: e.to_string(),
                }
            })?;
            let normalized = crate::config::expr::normalize_expr_tags(&text);
            let mut included: Vec<Entry> = serde_yaml::from_str(&normalized).map_err(|e| {
                crate::error::ComposeError::BadInclude {
                    path: path.clone(),
                    layer: layer.clone(),
                    detail: e.to_string(),
                }
            })?;
            let child_base = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            stack.push(key);
            graft_includes(&mut included, &child_base, stack, layer)?;
            stack.pop();
            // Grafted into `group`, at PARSE time: the included ids are ordinary rows of the tree
            // from here on, so a later layer patches them by id like any other (Decision D19).
            row.group.extend(included);
        }
        graft_includes(&mut row.group, base, stack, layer)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "bough-wp4-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn entry_roundtrips() {
        let yaml = r#"
- id: ledger
  plugin: ledger-sqlite
  config:
    path: /tmp/x.db
    wal: true
  disabled: false
  isolate:
    ledger: main
  inject: [clock]
  group:
    - id: ledger.child
      plugin: noop
"#;
        let rows = parse_entries(yaml, std::path::Path::new(".")).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, EntryId::new("ledger"));
        assert_eq!(rows[0].plugin.as_deref(), Some("ledger-sqlite"));
        assert_eq!(rows[0].group.len(), 1);
        assert!(rows[0].inject.declares("clock"));

        // Serialize and parse again: the same tree comes back.
        let out = serde_yaml::to_string(&rows).unwrap();
        let again = parse_entries(&out, std::path::Path::new(".")).unwrap();
        assert_eq!(rows, again);
    }

    #[test]
    fn unknown_field_is_rejected() {
        // `deny_unknown_fields` is the point: a typo in a bundle must be loud (§0.2).
        let err = parse_entries("- id: a\n  plugn: oops\n", std::path::Path::new("."))
            .expect_err("typo must not be silently ignored");
        assert!(format!("{err}").contains("plugn"), "{err}");
    }

    #[test]
    fn inject_list_form() {
        let rows = parse_entries(
            "- id: a\n  inject: [ledger, clock]\n",
            std::path::Path::new("."),
        )
        .unwrap();
        assert_eq!(
            rows[0].inject.required.iter().cloned().collect::<Vec<_>>(),
            vec!["clock".to_string(), "ledger".to_string()]
        );
        assert!(rows[0].inject.optional.is_empty());
    }

    #[test]
    fn inject_map_form() {
        let rows = parse_entries(
            "- id: a\n  inject:\n    required: [ledger]\n    optional: [tracer]\n",
            std::path::Path::new("."),
        )
        .unwrap();
        assert!(rows[0].inject.required.contains("ledger"));
        assert!(rows[0].inject.optional.contains("tracer"));
        assert!(!rows[0].inject.required.contains("tracer"));
    }

    #[test]
    fn union_keeps_a_plugin_static_requirement_required() {
        let entry = Inject::optional(["ledger"]);
        let static_ = Inject::required(["ledger"]);
        let u = entry.union(&static_);
        assert!(u.required.contains("ledger"));
        assert!(!u.optional.contains("ledger"));
    }

    #[test]
    fn include_is_grafted_at_parse_time() {
        let dir = tmpdir("include");
        write(&dir, "extra.yml", "- id: included-row\n  plugin: noop\n");
        let rows = parse_entries("- id: host\n  include: extra.yml\n", &dir).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].include.is_none(),
            "include is consumed at parse time"
        );
        // Grafted BEFORE any patch layer, so a later layer can patch `included-row` by id.
        assert_eq!(rows[0].group[0].id, EntryId::new("included-row"));
    }

    #[test]
    fn include_cycle_is_an_error() {
        let dir = tmpdir("cycle");
        write(&dir, "a.yml", "- id: a\n  include: b.yml\n");
        write(&dir, "b.yml", "- id: b\n  include: a.yml\n");
        let err = parse_entries("- id: root\n  include: a.yml\n", &dir)
            .expect_err("a cycle must not recurse forever");
        assert!(format!("{err}").contains("cycle"), "{err}");
    }
}
