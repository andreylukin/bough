//! Invariant: ONE renderer (Decision D9). `--dump-config` prints `render(&composition)` and the V6
//! test prints `render(&kernel.composition())`; there is no second formatter, because a second one
//! is how a dump starts lying about what booted.
//!
//! And ONE redaction, in that one renderer: a resolved secret never appears in a dump; the raw
//! expression that produced it (`config_raw`) stays verbatim, because it holds the expression,
//! not the value.

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
    m.insert(str_v("config"), redact(row.config.clone()));
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

/// Would a value under this field name be a secret? Names only — the composed tree carries no
/// schema and no sensitivity annotation, so the field name is the one honest signal available.
/// The carve-outs are forced by the shipped bundles: `*_env` names a VARIABLE (`api_key_env`),
/// and `*_tokens` counts tokens (`budget_tokens`, `default_max_tokens`, `map_max_tokens`, ...).
pub fn secret_field(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if n.ends_with("_env") || n.ends_with("_tokens") {
        return false;
    }
    matches!(n.as_str(), "api_key" | "token" | "secret" | "password")
        || n.ends_with("_key")
        || n.ends_with("_token")
        || n.ends_with("_secret")
        || n.ends_with("_password")
}

const REDACTED: &str = "«redacted»";

/// Mask resolved secret values in a config body. Only non-empty STRINGS are masked: an empty
/// `api_key` is a fact worth seeing, and a number under a secret-shaped name is a bug to show,
/// not a credential to hide. Public because every OTHER surface that shows a composed `config`
/// (the TUI's panel) must mask through the SAME pass — a second predicate is how two surfaces
/// start disagreeing about what is secret.
pub fn redact(v: serde_yaml::Value) -> serde_yaml::Value {
    use serde_yaml::Value as V;
    match v {
        V::Mapping(m) => V::Mapping(
            m.into_iter()
                .map(|(k, val)| {
                    let masked = match (&k, &val) {
                        (V::String(name), V::String(s)) if secret_field(name) && !s.is_empty() => {
                            str_v(REDACTED)
                        }
                        _ => redact(val),
                    };
                    (k, masked)
                })
                .collect(),
        ),
        V::Sequence(items) => V::Sequence(items.into_iter().map(redact).collect()),
        V::Tagged(t) => {
            let mut t = *t;
            t.value = redact(t.value);
            V::Tagged(Box::new(t))
        }
        other => other,
    }
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
    fn resolved_secrets_are_redacted_and_raw_expressions_stay() {
        let cat = TestCatalog::default().with("actions-linear", &["api_key", "api_key_env", "max_tokens", "endpoint"]);
        let mut c = Composer::new(&cat, ExprEnv::empty("tui").with_var("LINEAR_API_KEY", "lin_live_sekrit"));
        c.layer(
            LayerId::new("bundle:bough-base"),
            Patch::parse(concat!(
                "- id: actions.linear\n",
                "  plugin: actions-linear\n",
                "  config:\n",
                "    api_key: !!expr env(\"LINEAR_API_KEY\")\n",
                "    api_key_env: LINEAR_API_KEY\n",
                "    max_tokens: 2048\n",
                "    endpoint: \"\"\n",
            ))
            .unwrap(),
        );
        let comp = c.compose().unwrap();
        for format in [DumpFormat::Yaml, DumpFormat::Json] {
            let out = render(&comp, format);
            // The resolved value never appears; the mask and the raw expression do.
            assert!(!out.contains("lin_live_sekrit"), "{out}");
            assert!(out.contains(REDACTED), "{out}");
            assert!(out.contains("LINEAR_API_KEY"), "{out}");
            // A variable NAME under `_env` and a token COUNT stay readable.
            assert!(out.contains("api_key_env"), "{out}");
            assert!(out.contains("2048"), "{out}");
        }
    }

    #[test]
    fn an_empty_secret_string_is_a_fact_not_a_secret() {
        let cat = TestCatalog::default().with("actions-linear", &["api_key"]);
        let mut c = Composer::new(&cat, ExprEnv::empty("tui"));
        c.layer(
            LayerId::new("bundle:b"),
            Patch::parse("- id: a\n  plugin: actions-linear\n  config:\n    api_key: \"\"\n").unwrap(),
        );
        let comp = c.compose().unwrap();
        let yaml = render(&comp, DumpFormat::Yaml);
        assert!(!yaml.contains(REDACTED), "{yaml}");
        assert!(yaml.contains("api_key: ''"), "{yaml}");
    }

    #[test]
    fn secret_field_predicate_matches_the_shipped_bundles() {
        for masked in ["api_key", "token", "secret", "password", "gh_key", "access_token", "client_secret", "db_password", "API_KEY"] {
            assert!(secret_field(masked), "{masked} should mask");
        }
        for visible in ["api_key_env", "budget_tokens", "max_tokens", "default_max_tokens", "map_max_tokens", "reduce_max_tokens", "distill_max_tokens", "endpoint", "tokens", "path"] {
            assert!(!secret_field(visible), "{visible} should stay visible");
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
