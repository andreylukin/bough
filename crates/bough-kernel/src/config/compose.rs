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
use crate::plugin::ErasedPlugin;

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
        use sha2::Digest as _;
        let mut canon = String::new();
        canon_rows(tree, &mut canon);
        let mut h = sha2::Sha256::new();
        h.update(canon.as_bytes());
        Fingerprint(format!("{:x}", h.finalize()))
    }
    /// The canonical text the hash is taken over. Exposed for tests and for the dump.
    pub fn canonical_text(tree: &[Entry]) -> String {
        let mut s = String::new();
        canon_rows(tree, &mut s);
        s
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

/// What the composer needs from a catalog: a name lookup.
///
/// A trait rather than `&Catalog` so a unit test can compose against two hand-written plugins
/// instead of the whole linked binary; `&Catalog` still coerces at every real call site.
pub trait PluginLookup {
    fn lookup(&self, name: &str) -> Option<&dyn ErasedPlugin>;
}

impl PluginLookup for Catalog {
    fn lookup(&self, name: &str) -> Option<&dyn ErasedPlugin> {
        self.get(name)
    }
}

/// Stacks layers in order over an empty root.
pub struct Composer<'a> {
    catalog: &'a dyn PluginLookup,
    env: ExprEnv,
    layers: Vec<(LayerId, Patch)>,
}

impl<'a> Composer<'a> {
    /// Bind a catalog (for schema validation) and an expression environment.
    pub fn new(catalog: &'a dyn PluginLookup, env: ExprEnv) -> Self {
        Composer {
            catalog,
            env,
            layers: Vec::new(),
        }
    }
    /// Push one layer. Order is normative (§0.5): bundles, profile patch, user patch, `--patch`.
    pub fn layer(&mut self, id: LayerId, patch: Patch) -> &mut Self {
        self.layers.push((id, patch));
        self
    }
    /// Apply layers in order, evaluate `!!expr`, then validate EVERY row: an unknown plugin name
    /// is an error, a config the plugin's schema or `validate` rejects is an error naming the row,
    /// and the first bad row rejects the whole candidate. A patch naming an absent row id is a
    /// WARNING (§0.2).
    pub fn compose(self) -> Result<Composition, ComposeError> {
        let mut rows: Vec<Entry> = Vec::new();
        let mut provenance: BTreeMap<EntryId, RowProvenance> = BTreeMap::new();
        let mut warnings: Vec<ComposeWarning> = Vec::new();
        let mut layer_ids: Vec<LayerId> = Vec::new();

        for (layer, patch) in &self.layers {
            layer_ids.push(layer.clone());
            for id in patch.absent_ids(&rows) {
                warnings.push(ComposeWarning::AbsentRowId {
                    layer: layer.clone(),
                    id,
                });
            }
            let mut before = std::collections::BTreeSet::new();
            crate::config::patch::collect_ids(&rows, &mut before);

            {
                let layer = layer.clone();
                let prov = &mut provenance;
                let mut record = |id: &EntryId, field: &'static str| {
                    prov.entry(id.clone())
                        .or_insert_with(|| RowProvenance {
                            created_by: layer.clone(),
                            fields: BTreeMap::new(),
                        })
                        .fields
                        .insert(field, layer.clone());
                };
                patch.apply(&mut rows, &mut record);
            }

            let mut after = std::collections::BTreeSet::new();
            crate::config::patch::collect_ids(&rows, &mut after);
            for id in after.difference(&before) {
                // A row this layer created: every field of it is this layer's, and so is the row.
                let mut fields = BTreeMap::new();
                for f in ROW_FIELDS {
                    fields.insert(*f, layer.clone());
                }
                provenance.insert(
                    id.clone(),
                    RowProvenance {
                        created_by: layer.clone(),
                        fields,
                    },
                );
            }
            // A row this layer removed keeps no provenance.
            provenance.retain(|id, _| after.contains(id));
        }

        let raw = rows.clone();
        let last_layer = layer_ids
            .last()
            .cloned()
            .unwrap_or_else(|| LayerId::new("<empty>"));
        let tree = evaluate_rows(&raw, &self.env).map_err(|source| ComposeError::BadExpr {
            layer: last_layer.clone(),
            source,
        })?;

        validate_rows(&tree, self.catalog, &provenance, &last_layer)?;

        let fingerprint = Fingerprint::of(&tree);
        Ok(Composition {
            tree,
            raw,
            provenance,
            layers: layer_ids,
            warnings,
            fingerprint,
        })
    }
}

/// The field names provenance is tracked for.
pub const ROW_FIELDS: &[&str] = &["plugin", "config", "disabled", "isolate", "inject", "group"];

fn evaluate_rows(
    rows: &[Entry],
    env: &ExprEnv,
) -> Result<Vec<Entry>, crate::config::expr::ExprError> {
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(Entry {
            id: r.id.clone(),
            plugin: r.plugin.clone(),
            config: crate::config::expr::evaluate_tree(&r.config, env)?,
            disabled: crate::config::Expr::Literal(r.disabled.eval(env)?),
            isolate: r.isolate.clone(),
            inject: r.inject.clone(),
            group: evaluate_rows(&r.group, env)?,
            include: None,
            critical: r.critical,
        });
    }
    Ok(out)
}

fn validate_rows(
    rows: &[Entry],
    catalog: &dyn PluginLookup,
    provenance: &BTreeMap<EntryId, RowProvenance>,
    fallback: &LayerId,
) -> Result<(), ComposeError> {
    for r in rows {
        let layer = provenance
            .get(&r.id)
            .map(|p| p.created_by.clone())
            .unwrap_or_else(|| fallback.clone());
        let Some(name) = &r.plugin else {
            // The mount path rejects a plugin-less row (`PluginFactory::parse` looks `""` up in
            // the catalog), so the composer must too — otherwise `--dump-config` exits 0 on a tree
            // that cannot boot, and the dump stops being what boots (§0.5, V6).
            return Err(ComposeError::MissingPlugin {
                entry: r.id.clone(),
                layer: layer.clone(),
            });
        };
        {
            let plugin = catalog
                .lookup(name)
                .ok_or_else(|| ComposeError::UnknownPlugin {
                    entry: r.id.clone(),
                    plugin: name.clone(),
                    layer: layer.clone(),
                })?;
            plugin
                .parse(&r.config)
                .map_err(|source| ComposeError::BadConfig {
                    entry: r.id.clone(),
                    plugin: name.clone(),
                    layer: layer.clone(),
                    source,
                })?;
        }
        validate_rows(&r.group, catalog, provenance, fallback)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// canonical form (Decision D9)
// ---------------------------------------------------------------------------

fn canon_rows(rows: &[Entry], out: &mut String) {
    out.push('[');
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        canon_row(r, out);
    }
    out.push(']');
}

fn canon_row(r: &Entry, out: &mut String) {
    out.push('{');
    out.push_str("\"id\":");
    canon_str(r.id.as_str(), out);
    out.push_str(",\"plugin\":");
    match &r.plugin {
        Some(p) => canon_str(p, out),
        None => out.push_str("null"),
    }
    out.push_str(",\"config\":");
    canon_yaml(&r.config, out);
    out.push_str(",\"disabled\":");
    // The tree is EVALUATED, so `disabled` is a resolved bool here; an unresolved source would be
    // a composer bug and is hashed as its text rather than silently dropped.
    match &r.disabled {
        crate::config::Expr::Literal(b) => out.push_str(if *b { "true" } else { "false" }),
        crate::config::Expr::Source(s) => canon_str(s, out),
    }
    out.push_str(",\"isolate\":{");
    for (i, (k, v)) in r.isolate.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        canon_str(k, out);
        out.push(':');
        canon_str(v.as_str(), out);
    }
    out.push_str("},\"inject\":{\"required\":[");
    for (i, k) in r.inject.required.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        canon_str(k, out);
    }
    out.push_str("],\"optional\":[");
    for (i, k) in r.inject.optional.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        canon_str(k, out);
    }
    out.push_str("]},\"group\":");
    canon_rows(&r.group, out);
    out.push('}');
}

fn canon_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Canonical JSON for a YAML value: map keys sorted by their canonical key text, so the hash is
/// blind to key order and to comments.
fn canon_yaml(v: &serde_yaml::Value, out: &mut String) {
    use serde_yaml::Value as V;
    match v {
        V::Null => out.push_str("null"),
        V::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        V::Number(n) => out.push_str(&n.to_string()),
        V::String(s) => canon_str(s, out),
        V::Sequence(items) => {
            out.push('[');
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canon_yaml(it, out);
            }
            out.push(']');
        }
        V::Mapping(m) => {
            let mut pairs: Vec<(String, &V)> = m
                .iter()
                .map(|(k, val)| {
                    let mut ks = String::new();
                    canon_yaml(k, &mut ks);
                    (ks, val)
                })
                .collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            out.push('{');
            for (i, (k, val)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(k);
                out.push(':');
                canon_yaml(val, out);
            }
            out.push('}');
        }
        V::Tagged(t) => {
            out.push_str("[\"!\",");
            canon_str(t.tag.to_string().trim_start_matches('!'), out);
            out.push(',');
            canon_yaml(&t.value, out);
            out.push(']');
        }
    }
}

#[cfg(test)]
pub(crate) mod testkit {
    //! A hand-written `ErasedPlugin` so composition can be unit-tested without the linked
    //! binary's catalog and without `Shim` (WP-3).
    use super::*;
    use crate::context::Context;
    use crate::error::{ConfigError, PluginError};
    use crate::plugin::{ErasedConfig, Reconfigure};
    use std::collections::BTreeMap as Map;

    pub struct TestPlugin {
        pub name: &'static str,
        /// Config keys this plugin accepts. A key outside the set is rejected, which stands in
        /// for schema validation.
        pub allowed: &'static [&'static str],
    }

    impl ErasedPlugin for TestPlugin {
        fn name(&self) -> &'static str {
            self.name
        }
        fn inject(&self) -> crate::config::Inject {
            crate::config::Inject::none()
        }
        fn schema(&self) -> schemars::Schema {
            schemars::json_schema!({ "type": "object" })
        }
        fn parse(&self, raw: &serde_yaml::Value) -> Result<ErasedConfig, ConfigError> {
            if let serde_yaml::Value::Mapping(m) = raw {
                for k in m.keys() {
                    let k = k.as_str().unwrap_or_default();
                    if !self.allowed.contains(&k) {
                        return Err(ConfigError::Schema {
                            detail: format!("unknown config key `{k}`"),
                        });
                    }
                }
            }
            Ok(ErasedConfig::new((), raw.clone()))
        }
        fn apply(
            &self,
            _ctx: Context,
            _cfg: ErasedConfig,
        ) -> futures::future::BoxFuture<'static, Result<(), PluginError>> {
            Box::pin(async { Ok(()) })
        }
        fn reconfigure(
            &self,
            _ctx: &Context,
            _old: &ErasedConfig,
            _new: &ErasedConfig,
        ) -> Reconfigure {
            Reconfigure::Reload
        }
        fn invariants(&self) -> Vec<crate::invariant::InvariantSpec> {
            Vec::new()
        }
    }

    #[derive(Default)]
    pub struct TestCatalog(Map<&'static str, TestPlugin>);

    impl TestCatalog {
        pub fn with(mut self, name: &'static str, allowed: &'static [&'static str]) -> Self {
            self.0.insert(name, TestPlugin { name, allowed });
            self
        }
    }

    impl PluginLookup for TestCatalog {
        fn lookup(&self, name: &str) -> Option<&dyn ErasedPlugin> {
            self.0.get(name).map(|p| p as &dyn ErasedPlugin)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::TestCatalog;
    use super::*;
    use crate::config::patch::Patch;

    fn cat() -> TestCatalog {
        TestCatalog::default()
            .with("ledger-sqlite", &["path", "wal"])
            .with("noop", &["who"])
    }

    fn env() -> ExprEnv {
        ExprEnv::empty("tui")
    }

    fn compose_layers(pairs: &[(&str, &str)], catalog: &TestCatalog, env: ExprEnv) -> Composition {
        try_compose(pairs, catalog, env).expect("composes")
    }

    fn try_compose(
        pairs: &[(&str, &str)],
        catalog: &TestCatalog,
        env: ExprEnv,
    ) -> Result<Composition, ComposeError> {
        let mut c = Composer::new(catalog, env);
        for (id, yaml) in pairs {
            c.layer(LayerId::new(*id), Patch::parse(yaml).unwrap());
        }
        c.compose()
    }

    const BASE: &str = "- id: ledger\n  plugin: ledger-sqlite\n  config:\n    path: /a\n";

    /// §0.5, V6: `--dump-config` renders exactly what boots. The mount path rejects a row that
    /// names no plugin, so the composer must reject it too — a dump that exits 0 on a tree the
    /// kernel refuses is a dump that lies.
    #[test]
    fn a_row_naming_no_plugin_is_rejected_by_the_composer() {
        let err = try_compose(&[("bundle:b", "- id: pure.group\n")], &cat(), env())
            .expect_err("a plugin-less row must not compose");
        assert!(
            matches!(err, ComposeError::MissingPlugin { ref entry, .. } if entry.as_str() == "pure.group"),
            "{err}"
        );
        assert!(err.to_string().contains("D18"), "{err}");
    }

    #[test]
    fn layers_apply_in_order() {
        let c = compose_layers(
            &[
                ("bundle:bough-base", BASE),
                (
                    "profile:tui",
                    "entries:\n  ledger:\n    config:\n      path: /b\n",
                ),
                ("user", "entries:\n  ledger:\n    config:\n      path: /c\n"),
            ],
            &cat(),
            env(),
        );
        assert_eq!(
            c.tree[0].config.get("path").unwrap().as_str().unwrap(),
            "/c"
        );
        assert_eq!(
            c.layers.iter().map(|l| l.to_string()).collect::<Vec<_>>(),
            vec!["bundle:bough-base", "profile:tui", "user"]
        );
    }

    #[test]
    fn provenance_names_the_last_writing_layer_per_field() {
        let c = compose_layers(
            &[
                ("bundle:bough-base", BASE),
                (
                    "profile:tui",
                    "entries:\n  ledger:\n    config:\n      path: /b\n",
                ),
                ("user", "entries:\n  ledger:\n    disabled: true\n"),
            ],
            &cat(),
            env(),
        );
        let p = c.provenance.get(&EntryId::new("ledger")).unwrap();
        assert_eq!(p.created_by, LayerId::new("bundle:bough-base"));
        assert_eq!(p.fields["config"], LayerId::new("profile:tui"));
        assert_eq!(p.fields["disabled"], LayerId::new("user"));
        // A field nobody rewrote still belongs to the layer that created the row.
        assert_eq!(p.fields["inject"], LayerId::new("bundle:bough-base"));
    }

    #[test]
    fn unknown_plugin_name_is_a_compose_error() {
        let err = try_compose(
            &[("bundle:bough-base", "- id: x\n  plugin: not-a-plugin\n")],
            &cat(),
            env(),
        )
        .expect_err("an unknown plugin name must fail the candidate");
        assert!(matches!(
            &err,
            ComposeError::UnknownPlugin { entry, plugin, .. }
                if entry.as_str() == "x" && plugin == "not-a-plugin"
        ));
    }

    #[test]
    fn bad_config_is_a_compose_error_naming_the_row() {
        let err = try_compose(
            &[(
                "bundle:bough-base",
                "- id: ledger\n  plugin: ledger-sqlite\n  config:\n    typo: 1\n",
            )],
            &cat(),
            env(),
        )
        .expect_err("a config the plugin rejects fails the whole candidate");
        let text = format!("{err}");
        assert!(text.contains("ledger"), "{text}");
        assert!(
            matches!(&err, ComposeError::BadConfig { entry, .. } if entry.as_str() == "ledger")
        );
    }

    #[test]
    fn fingerprint_is_stable_across_key_order_and_comments() {
        let a = compose_layers(
            &[(
                "b",
                "- id: ledger\n  plugin: ledger-sqlite\n  config:\n    path: /a\n    wal: true\n",
            )],
            &cat(),
            env(),
        );
        let b = compose_layers(
            &[(
                "b",
                "# a comment the hash must not see\n- id: ledger\n  plugin: ledger-sqlite\n  config:\n    wal: true   # trailing note\n    path: /a\n",
            )],
            &cat(),
            env(),
        );
        assert_eq!(a.fingerprint, b.fingerprint);
    }

    #[test]
    fn fingerprint_changes_when_a_row_config_changes() {
        let a = compose_layers(&[("b", BASE)], &cat(), env());
        let b = compose_layers(
            &[
                ("b", BASE),
                (
                    "profile:tui",
                    "entries:\n  ledger:\n    config:\n      path: /z\n",
                ),
            ],
            &cat(),
            env(),
        );
        assert_ne!(a.fingerprint, b.fingerprint);
    }

    #[test]
    fn fingerprint_changes_when_disabled_flips() {
        let a = compose_layers(&[("b", BASE)], &cat(), env());
        let b = compose_layers(
            &[
                ("b", BASE),
                ("user", "entries:\n  ledger:\n    disabled: true\n"),
            ],
            &cat(),
            env(),
        );
        assert_ne!(a.fingerprint, b.fingerprint);
    }

    #[test]
    fn fingerprint_is_computed_after_expr_evaluation() {
        let yaml =
            "- id: ledger\n  plugin: ledger-sqlite\n  config:\n    path: !!expr env(\"DB\")\n";
        let a = compose_layers(&[("b", yaml)], &cat(), env().with_var("DB", "/one"));
        let b = compose_layers(&[("b", yaml)], &cat(), env().with_var("DB", "/two"));
        // Same YAML, different environment: the fingerprint is of the tree that would be LIVE.
        assert_ne!(a.fingerprint, b.fingerprint);

        let literal = compose_layers(
            &[(
                "b",
                "- id: ledger\n  plugin: ledger-sqlite\n  config:\n    path: /one\n",
            )],
            &cat(),
            env(),
        );
        assert_eq!(a.fingerprint, literal.fingerprint);
        // The raw tree keeps the expression so the dump can show both.
        assert!(matches!(
            a.raw[0].config.get("path"),
            Some(serde_yaml::Value::Tagged(_))
        ));
    }

    #[test]
    fn absent_row_id_is_a_warning_and_the_candidate_still_composes() {
        let c = compose_layers(
            &[
                ("b", BASE),
                ("user", "entries:\n  ghost:\n    disabled: true\n"),
            ],
            &cat(),
            env(),
        );
        assert_eq!(c.warnings.len(), 1);
        assert!(matches!(
            &c.warnings[0],
            ComposeWarning::AbsentRowId { id, layer }
                if id.as_str() == "ghost" && layer.as_str() == "user"
        ));
    }
}
