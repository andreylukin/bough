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
#[derive(Clone, Debug, Default)]
pub struct Patch {
    pub entries: BTreeMap<EntryId, EntryPatch>,
    pub insert: Vec<Insert>,
    /// Not in §0.5; here because a profile must be able to drop a base row without knowing what
    /// `disabled` would leave behind (Decision D8).
    pub remove: Vec<EntryId>,
}

impl Patch {
    /// Parse one layer document.
    pub fn parse(yaml: &str) -> Result<Patch, serde_yaml::Error> {
        serde_yaml::from_str(&crate::config::expr::normalize_expr_tags(yaml))
    }
    /// Apply this layer to `rows` in place, recording provenance through `record`.
    ///
    /// The whole algorithm is ~200 lines and has no merge in it (§0.5).
    pub fn apply(&self, rows: &mut Vec<Entry>, record: &mut dyn FnMut(&EntryId, &'static str)) {
        // Inserts first, so a layer may create a row and patch it in the same document.
        for ins in &self.insert {
            insert_at(rows, &ins.at, ins.entry.clone());
        }
        for (id, ep) in &self.entries {
            if let Some(row) = find_mut(rows, id) {
                // Every field is REPLACED. There is no merge here and there must never be one:
                // §0.5 says restate the fields you keep.
                if let Some(cfg) = &ep.config {
                    row.config = cfg.clone();
                    record(id, "config");
                }
                if let Some(p) = &ep.plugin {
                    row.plugin = Some(p.clone());
                    record(id, "plugin");
                }
                if let Some(d) = &ep.disabled {
                    row.disabled = d.clone();
                    record(id, "disabled");
                }
                if let Some(iso) = &ep.isolate {
                    row.isolate = iso.clone();
                    record(id, "isolate");
                }
                if let Some(inj) = &ep.inject {
                    row.inject = inj.clone();
                    record(id, "inject");
                }
            }
        }
        for id in &self.remove {
            remove_by_id(rows, id);
        }
    }
    /// Row ids this layer names that `rows` does not contain and this layer does not itself create.
    pub fn absent_ids(&self, rows: &[Entry]) -> Vec<EntryId> {
        let mut present: std::collections::BTreeSet<EntryId> = std::collections::BTreeSet::new();
        collect_ids(rows, &mut present);
        for ins in &self.insert {
            present.insert(ins.entry.id.clone());
            collect_ids(std::slice::from_ref(&ins.entry), &mut present);
        }
        self.entries
            .keys()
            .chain(self.remove.iter())
            .filter(|id| !present.contains(*id))
            .cloned()
            .collect()
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
///
/// YAML: `{ before: some-id, entry: {...} }`, `after:`, `into:`, or no anchor at all for the root
/// end. Deserialized through [`InsertRepr`] rather than `#[serde(flatten)]` + `#[serde(other)]`:
/// serde allows `other` only on an internally tagged enum, and a flattened externally tagged enum
/// cannot express "no anchor key at all" (deviation noted in the WP-4 report).
#[derive(Clone, Debug)]
pub struct Insert {
    pub at: InsertAt,
    pub entry: Entry,
}

/// Insertion position.
#[derive(Clone, Debug, PartialEq)]
pub enum InsertAt {
    Before(EntryId),
    After(EntryId),
    /// Into the named row's `group`.
    Into(EntryId),
    RootEnd,
}

/// The serde-facing shape of [`Insert`].
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InsertRepr {
    #[serde(default)]
    pub before: Option<EntryId>,
    #[serde(default)]
    pub after: Option<EntryId>,
    #[serde(default)]
    pub into: Option<EntryId>,
    pub entry: Entry,
}

impl<'de> serde::Deserialize<'de> for Insert {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let r = InsertRepr::deserialize(d)?;
        let anchors = [&r.before, &r.after, &r.into]
            .iter()
            .filter(|a| a.is_some())
            .count();
        if anchors > 1 {
            return Err(D::Error::custom(
                "insert names more than one of before/after/into",
            ));
        }
        let at = if let Some(id) = r.before {
            InsertAt::Before(id)
        } else if let Some(id) = r.after {
            InsertAt::After(id)
        } else if let Some(id) = r.into {
            InsertAt::Into(id)
        } else {
            InsertAt::RootEnd
        };
        Ok(Insert { at, entry: r.entry })
    }
}

/// The map form of a layer document.
///
/// NOT an `#[serde(untagged)]` enum over "rows or doc": serde's untagged buffering cannot carry a
/// YAML tagged scalar, so an `!!expr` anywhere inside a patch would fail to deserialize. [`Patch`]
/// therefore has a hand-written `Deserialize` that dispatches on the document's shape (deviation
/// noted in the WP-4 report).
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchRepr {
    #[serde(default)]
    pub entries: BTreeMap<EntryId, EntryPatch>,
    #[serde(default)]
    pub insert: Vec<Insert>,
    #[serde(default)]
    pub remove: Vec<EntryId>,
}

impl From<PatchRepr> for Patch {
    fn from(r: PatchRepr) -> Self {
        Patch {
            entries: r.entries,
            insert: r.insert,
            remove: r.remove,
        }
    }
}

impl<'de> serde::Deserialize<'de> for Patch {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let v = serde_yaml::Value::deserialize(d)?;
        match v {
            // A bare sequence is sugar for inserting those rows at the root end, which is what
            // lets `bundles/bough-base.yml` read as a plain row list.
            serde_yaml::Value::Sequence(_) => {
                let rows: Vec<Entry> = serde_yaml::from_value(v).map_err(D::Error::custom)?;
                Ok(Patch {
                    entries: BTreeMap::new(),
                    insert: rows
                        .into_iter()
                        .map(|entry| Insert {
                            at: InsertAt::RootEnd,
                            entry,
                        })
                        .collect(),
                    remove: Vec::new(),
                })
            }
            serde_yaml::Value::Null => Ok(Patch::default()),
            other => {
                let repr: PatchRepr = serde_yaml::from_value(other).map_err(D::Error::custom)?;
                Ok(repr.into())
            }
        }
    }
}

/// Every row id in `rows`, children included.
pub(crate) fn collect_ids(rows: &[Entry], out: &mut std::collections::BTreeSet<EntryId>) {
    for r in rows {
        out.insert(r.id.clone());
        collect_ids(&r.group, out);
    }
}

/// Depth-first search for a row by id. Written through [`position_of`] because a directly
/// recursive `&mut` search does not pass the borrow checker without polonius.
fn find_mut<'a>(rows: &'a mut Vec<Entry>, id: &EntryId) -> Option<&'a mut Entry> {
    let (path, idx) = position_of(rows, id)?;
    Some(&mut parent_vec(rows, &path)[idx])
}

/// Drop a row and, with it, its whole `group`.
fn remove_by_id(rows: &mut Vec<Entry>, id: &EntryId) -> bool {
    if let Some(pos) = rows.iter().position(|r| &r.id == id) {
        rows.remove(pos);
        return true;
    }
    for r in rows.iter_mut() {
        if remove_by_id(&mut r.group, id) {
            return true;
        }
    }
    false
}

/// Place `entry` at `at`. An anchor that does not exist falls back to the root end, which is
/// reported separately as a [`crate::config::ComposeWarning::AbsentRowId`].
fn insert_at(rows: &mut Vec<Entry>, at: &InsertAt, entry: Entry) {
    match at {
        InsertAt::RootEnd => rows.push(entry),
        InsertAt::Before(anchor) => match position_of(rows, anchor) {
            Some((parent, idx)) => parent_vec(rows, &parent).insert(idx, entry),
            None => rows.push(entry),
        },
        InsertAt::After(anchor) => match position_of(rows, anchor) {
            Some((parent, idx)) => parent_vec(rows, &parent).insert(idx + 1, entry),
            None => rows.push(entry),
        },
        InsertAt::Into(anchor) => match find_mut(rows, anchor) {
            Some(row) => row.group.push(entry),
            None => rows.push(entry),
        },
    }
}

/// The path of child indices to the vector holding `id`, plus the index within it.
fn position_of(rows: &[Entry], id: &EntryId) -> Option<(Vec<usize>, usize)> {
    for (i, r) in rows.iter().enumerate() {
        if &r.id == id {
            return Some((Vec::new(), i));
        }
        if let Some((mut path, idx)) = position_of(&r.group, id) {
            path.insert(0, i);
            return Some((path, idx));
        }
    }
    None
}

fn parent_vec<'a>(rows: &'a mut Vec<Entry>, path: &[usize]) -> &'a mut Vec<Entry> {
    let mut cur = rows;
    for &i in path {
        cur = &mut cur[i].group;
    }
    cur
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(yaml: &str) -> Vec<Entry> {
        crate::config::entry::parse_entries(yaml, std::path::Path::new(".")).unwrap()
    }

    fn apply(patch: &Patch, rows: &mut Vec<Entry>) -> Vec<(String, &'static str)> {
        let mut log = Vec::new();
        patch.apply(rows, &mut |id, field| log.push((id.to_string(), field)));
        log
    }

    fn get<'a>(rows: &'a [Entry], id: &str) -> Option<&'a Entry> {
        for r in rows {
            if r.id.as_str() == id {
                return Some(r);
            }
            if let Some(f) = get(&r.group, id) {
                return Some(f);
            }
        }
        None
    }

    #[test]
    fn config_is_replaced_not_merged() {
        let mut tree = rows("- id: ledger\n  plugin: p\n  config:\n    path: /a\n    wal: true\n");
        let patch = Patch::parse("entries:\n  ledger:\n    config:\n      path: /b\n").unwrap();
        let log = apply(&patch, &mut tree);
        let cfg = &get(&tree, "ledger").unwrap().config;
        assert_eq!(cfg.get("path").unwrap().as_str().unwrap(), "/b");
        // NO deep merge (§0.5): `wal` is gone, because the layer restated the whole config.
        assert!(
            cfg.get("wal").is_none(),
            "config must be replaced wholesale"
        );
        assert_eq!(log, vec![("ledger".to_string(), "config")]);
    }

    #[test]
    fn disabled_can_be_set_by_patch() {
        let mut tree = rows("- id: tui\n  plugin: p\n");
        assert_eq!(
            get(&tree, "tui").unwrap().disabled,
            crate::config::Expr::Literal(false)
        );
        let patch = Patch::parse("entries:\n  tui:\n    disabled: true\n").unwrap();
        apply(&patch, &mut tree);
        assert_eq!(
            get(&tree, "tui").unwrap().disabled,
            crate::config::Expr::Literal(true)
        );

        let patch =
            Patch::parse("entries:\n  tui:\n    disabled: !!expr profile() == \"headless\"\n")
                .unwrap();
        apply(&patch, &mut tree);
        assert!(matches!(
            get(&tree, "tui").unwrap().disabled,
            crate::config::Expr::Source(_)
        ));
    }

    #[test]
    fn insert_before_after_into_and_root_end() {
        let mut tree = rows("- id: a\n- id: b\n  group:\n    - id: b1\n");
        let patch = Patch::parse(
            "insert:\n  - before: b\n    entry: {id: pre-b}\n  - after: a\n    entry: {id: post-a}\n  - into: b\n    entry: {id: b2}\n  - entry: {id: tail}\n",
        )
        .unwrap();
        apply(&patch, &mut tree);
        let top: Vec<&str> = tree.iter().map(|r| r.id.as_str()).collect();
        // `post-a` goes in right after `a`; `pre-b` right before `b`, which by then follows it.
        assert_eq!(top, vec!["a", "post-a", "pre-b", "b", "tail"]);
        let b = get(&tree, "b").unwrap();
        assert_eq!(
            b.group.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["b1", "b2"]
        );
    }

    #[test]
    fn remove_drops_the_row_and_its_group() {
        let mut tree = rows("- id: a\n  group:\n    - id: a1\n    - id: a2\n- id: b\n");
        let patch = Patch::parse("remove: [a]\n").unwrap();
        apply(&patch, &mut tree);
        assert!(get(&tree, "a").is_none());
        assert!(get(&tree, "a1").is_none(), "children go with the parent");
        assert!(get(&tree, "a2").is_none());
        assert!(get(&tree, "b").is_some());
    }

    #[test]
    fn absent_row_id_is_a_warning_not_an_error() {
        let tree = rows("- id: a\n");
        let patch = Patch::parse("entries:\n  ghost:\n    disabled: true\n").unwrap();
        let absent = patch.absent_ids(&tree);
        assert_eq!(absent, vec![EntryId::new("ghost")]);
        // Applying it is not an error; the row simply is not there.
        let mut tree2 = tree.clone();
        apply(&patch, &mut tree2);
        assert_eq!(tree2, tree);

        // A row the same layer inserts is NOT absent.
        let patch = Patch::parse(
            "insert:\n  - entry: {id: fresh}\nentries:\n  fresh:\n    disabled: true\n",
        )
        .unwrap();
        assert!(patch.absent_ids(&tree).is_empty());
    }

    #[test]
    fn bare_sequence_is_sugar_for_insert_at_root() {
        let patch = Patch::parse("- id: a\n  plugin: p\n- id: b\n").unwrap();
        assert!(patch.entries.is_empty());
        assert!(patch.remove.is_empty());
        assert_eq!(patch.insert.len(), 2);
        assert!(patch.insert.iter().all(|i| i.at == InsertAt::RootEnd));
        let mut tree = Vec::new();
        apply(&patch, &mut tree);
        assert_eq!(
            tree.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }
}
