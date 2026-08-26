//! Invariant: ONE renderer (Decision D9). `--dump-config` prints `render(&composition)` and the V6
//! test prints `render(&kernel.composition())`; there is no second formatter, because a second one
//! is how a dump starts lying about what booted.

use crate::config::compose::{Composition, ROW_FIELDS};
use crate::config::entry::Entry;
use crate::config::{ComposeWarning, Expr};

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
    let doc = annotate(c);
    match format {
        DumpFormat::Yaml => serde_yaml::to_string(&doc).expect("annotated dump is serialisable"),
        DumpFormat::Json => {
            let mut s = String::new();
            json(&doc, &mut s);
            s.push('\n');
            s
        }
    }
}

/// The annotated document both formats print. Built as a `serde_yaml::Value` so there is exactly
/// one shape and exactly one place that decides what a dump says.
fn annotate(c: &Composition) -> serde_yaml::Value {
    let mut root = serde_yaml::Mapping::new();
    root.insert(str_v("fingerprint"), str_v(c.fingerprint.as_str()));
    root.insert(
        str_v("layers"),
        serde_yaml::Value::Sequence(c.layers.iter().map(|l| str_v(l.as_str())).collect()),
    );
    root.insert(
        str_v("warnings"),
        serde_yaml::Value::Sequence(
            c.warnings
                .iter()
                .map(|w| match w {
                    ComposeWarning::AbsentRowId { layer, id } => str_v(&format!(
                        "layer `{layer}` patches row `{id}`, which no layer created"
                    )),
                })
                .collect(),
        ),
    );
    root.insert(str_v("rows"), rows_v(&c.tree, &c.raw, c));
    serde_yaml::Value::Mapping(root)
}

fn rows_v(tree: &[Entry], raw: &[Entry], c: &Composition) -> serde_yaml::Value {
    serde_yaml::Value::Sequence(
        tree.iter()
            .map(|row| {
                let raw_row = raw.iter().find(|r| r.id == row.id);
                row_v(row, raw_row, c)
            })
            .collect(),
    )
}

fn row_v(row: &Entry, raw: Option<&Entry>, c: &Composition) -> serde_yaml::Value {
    let mut m = serde_yaml::Mapping::new();
    m.insert(str_v("id"), str_v(row.id.as_str()));
    m.insert(
        str_v("plugin"),
        match &row.plugin {
            Some(p) => str_v(p),
            None => serde_yaml::Value::Null,
        },
    );
    m.insert(str_v("config"), row.config.clone());
    if let Some(r) = raw {
        if r.config != row.config {
            m.insert(str_v("config_raw"), r.config.clone());
        }
        if let Expr::Source(src) = &r.disabled {
            m.insert(str_v("disabled_raw"), str_v(src));
        }
    }
    m.insert(
        str_v("disabled"),
        match &row.disabled {
            Expr::Literal(b) => serde_yaml::Value::Bool(*b),
            Expr::Source(s) => str_v(s),
        },
    );
    let mut iso = serde_yaml::Mapping::new();
    for (k, v) in &row.isolate {
        iso.insert(str_v(k), str_v(v.as_str()));
    }
    m.insert(str_v("isolate"), serde_yaml::Value::Mapping(iso));
    let mut inj = serde_yaml::Mapping::new();
    inj.insert(
        str_v("required"),
        serde_yaml::Value::Sequence(row.inject.required.iter().map(|k| str_v(k)).collect()),
    );
    inj.insert(
        str_v("optional"),
        serde_yaml::Value::Sequence(row.inject.optional.iter().map(|k| str_v(k)).collect()),
    );
    m.insert(str_v("inject"), serde_yaml::Value::Mapping(inj));

    // The annotation §0.5 asks for: which layer last wrote each field of this row.
    let mut layers = serde_yaml::Mapping::new();
    if let Some(p) = c.provenance.get(&row.id) {
        layers.insert(str_v("created_by"), str_v(p.created_by.as_str()));
        for f in ROW_FIELDS {
            if let Some(l) = p.fields.get(f) {
                layers.insert(str_v(f), str_v(l.as_str()));
            }
        }
    }
    m.insert(str_v("layers"), serde_yaml::Value::Mapping(layers));

    let raw_group: &[Entry] = raw.map(|r| &r.group[..]).unwrap_or(&[]);
    m.insert(str_v("group"), rows_v(&row.group, raw_group, c));
    serde_yaml::Value::Mapping(m)
}

fn str_v(s: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(s.to_owned())
}

/// Insertion-ordered JSON. The document is built in a fixed order above, so this is deterministic
/// without sorting — and sorting here would hide the row order the tree actually has.
fn json(v: &serde_yaml::Value, out: &mut String) {
    use serde_yaml::Value as V;
    match v {
        V::Null => out.push_str("null"),
        V::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        V::Number(n) => out.push_str(&n.to_string()),
        V::String(s) => out.push_str(&serde_json::to_string(s).expect("string")),
        V::Sequence(items) => {
            out.push('[');
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json(it, out);
            }
            out.push(']');
        }
        V::Mapping(m) => {
            out.push('{');
            for (i, (k, val)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                let ks = match k {
                    V::String(s) => s.clone(),
                    other => serde_yaml::to_string(other)
                        .unwrap_or_default()
                        .trim()
                        .to_owned(),
                };
                out.push_str(&serde_json::to_string(&ks).expect("key"));
                out.push(':');
                json(val, out);
            }
            out.push('}');
        }
        V::Tagged(t) => json(&t.value, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::compose::testkit::TestCatalog;
    use crate::config::{Composer, ExprEnv, LayerId, Patch};

    fn composition() -> crate::config::Composition {
        let cat = TestCatalog::default().with("ledger-sqlite", &["path"]);
        let mut c = Composer::new(&cat, ExprEnv::empty("tui").with_var("DB", "/live.db"));
        c.layer(
            LayerId::new("bundle:bough-base"),
            Patch::parse("- id: ledger\n  plugin: ledger-sqlite\n  config:\n    path: /a\n")
                .unwrap(),
        );
        c.layer(
            LayerId::new("profile:tui"),
            Patch::parse("entries:\n  ledger:\n    config:\n      path: !!expr env(\"DB\")\n")
                .unwrap(),
        );
        c.compose().unwrap()
    }

    #[test]
    fn render_is_deterministic() {
        let c = composition();
        for format in [DumpFormat::Yaml, DumpFormat::Json] {
            let a = render(&c, format);
            let b = render(&c, format);
            assert_eq!(a, b);
            // And a second composition of the same layers renders identically.
            assert_eq!(a, render(&composition(), format));
        }
    }

    #[test]
    fn render_annotates_each_row_with_its_layer() {
        let c = composition();
        let yaml = render(&c, DumpFormat::Yaml);
        assert!(yaml.contains("created_by: bundle:bough-base"), "{yaml}");
        // `config` was last written by the profile layer, and both the raw expression and its
        // resolved value are shown.
        assert!(yaml.contains("config: profile:tui"), "{yaml}");
        assert!(yaml.contains("/live.db"), "{yaml}");
        assert!(yaml.contains("config_raw"), "{yaml}");
        assert!(yaml.contains(c.fingerprint.as_str()), "{yaml}");

        let json = render(&c, DumpFormat::Json);
        assert!(
            json.contains("\"created_by\":\"bundle:bough-base\""),
            "{json}"
        );
        assert!(json.contains(c.fingerprint.as_str()), "{json}");
    }
}
