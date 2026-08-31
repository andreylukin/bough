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
//!   → ~/.bough/bough.mcp.patch.yml (absent ⇒ skipped silently)               "mcp"
//!   → ~/.bough/bough.patch.yml (absent ⇒ skipped silently)                   "user"
//!   → ~/.bough/bough.ui.patch.yml (absent ⇒ skipped silently)                "ui"
//!   → each --patch FILE, in argument order                                   "patch:<n>:<file>"
//! ```
//!
//! The `mcp` layer is `bough sync-mcp`'s machine-written adoption of Claude Code's MCP grants
//! (`crate::syncmcp`). It sits BELOW the user patch: a machine's sync never outranks a person's
//! own `mcp.rmcp` entry.
//!
//! The `ui` layer is the panel's disabled-only toggle file (§0.5). It sits BEFORE the `--patch`
//! overlays on purpose: an explicit per-invocation flag outranks a persisted preference, and the
//! `scripts/tui/` fixtures that mount unbundled rows by `--patch` must keep working on a machine
//! whose panel once toggled something.

use std::path::PathBuf;

use bough_kernel::{Catalog, Composer, Composition, ExprEnv, LayerId, Patch};

use crate::cli::{BootError, Cli};
use crate::profile::{self, Profile, Sources};

/// One layer, located but not yet parsed.
///
/// Locating and parsing are separate so the ORDER of the stack — the part §0.5 makes normative —
/// is testable on its own, without a catalog or a kernel.
#[derive(Debug, Clone)]
pub struct Layer {
    pub id: LayerId,
    pub source: LayerSource,
    /// The directory this layer's `include:` paths resolve against — the directory the document
    /// was read from, or `$BOUGH_HOME` for an embedded one.
    pub base: PathBuf,
}

/// Where a layer's patch comes from.
#[derive(Debug, Clone)]
pub enum LayerSource {
    /// A YAML document read from disk or from the embedded copies.
    Text { origin: String, yaml: String },
    /// The profile document's own `patch:` block, already parsed with the profile.
    ProfilePatch,
    /// A layer the launcher built itself: `bough exec`'s one row-config write. It never comes
    /// from text, so a shell-quoted task cannot be re-parsed as YAML.
    Synthetic(bough_kernel::Patch),
}

/// The plan of layers, in the normative order, plus the profile that named them.
///
/// `--dump-config` and boot both go through here; nothing else stacks layers.
pub fn plan_layers(cli: &Cli) -> Result<(Profile, Sources, Vec<Layer>), BootError> {
    let root = cli.root.as_deref();
    // A subcommand selects the profile; `--profile` selects it otherwise (§0.1 item 2).
    let (profile, sources) = profile::resolve_profile(cli.effective_profile(), root)?;

    let mut layers = Vec::new();
    for b in &profile.bundles {
        let (yaml, origin) = profile::load_bundle_text(b, &profile.name, root)?;
        layers.push(Layer {
            id: LayerId::new(format!("bundle:{b}")),
            base: include_base(&origin),
            source: LayerSource::Text {
                origin: origin.to_string(),
                yaml,
            },
        });
    }

    layers.push(Layer {
        id: LayerId::new(format!("profile:{}", profile.name)),
        base: include_base(&sources.profile),
        source: LayerSource::ProfilePatch,
    });

    // The `mcp` layer: `bough sync-mcp`'s machine-written adoption of Claude Code's grants.
    // BEFORE the user patch on purpose — a person's own `mcp.rmcp` entry outranks the sync.
    let mcp = bough_util::mcp_patch_path();
    if mcp.is_file() {
        let yaml = std::fs::read_to_string(&mcp).map_err(|e| BootError::BadFile {
            path: mcp.clone(),
            detail: e.to_string(),
        })?;
        layers.push(Layer {
            id: LayerId::new("mcp"),
            base: parent_dir(&mcp),
            source: LayerSource::Text {
                origin: mcp.display().to_string(),
                yaml,
            },
        });
    }

    let user = bough_util::user_patch_path();
    if user.is_file() {
        let yaml = std::fs::read_to_string(&user).map_err(|e| BootError::BadFile {
            path: user.clone(),
            detail: e.to_string(),
        })?;
        layers.push(Layer {
            id: LayerId::new("user"),
            base: parent_dir(&user),
            source: LayerSource::Text {
                origin: user.display().to_string(),
                yaml,
            },
        });
    }

    let ui = bough_util::ui_patch_path();
    if ui.is_file() {
        let yaml = std::fs::read_to_string(&ui).map_err(|e| BootError::BadFile {
            path: ui.clone(),
            detail: e.to_string(),
        })?;
        layers.push(Layer {
            id: LayerId::new("ui"),
            base: parent_dir(&ui),
            source: LayerSource::Text {
                origin: ui.display().to_string(),
                yaml,
            },
        });
    }

    for (n, p) in cli.patches.iter().enumerate() {
        let yaml = std::fs::read_to_string(p).map_err(|e| BootError::BadFile {
            path: p.clone(),
            detail: e.to_string(),
        })?;
        layers.push(Layer {
            id: LayerId::new(format!("patch:{n}:{}", p.display())),
            base: parent_dir(p),
            source: LayerSource::Text {
                origin: p.display().to_string(),
                yaml,
            },
        });
    }

    Ok((profile, sources, layers))
}

/// The directory a document's relative `include:` paths resolve against.
fn parent_dir(p: &std::path::Path) -> PathBuf {
    p.parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// As [`parent_dir`], for a located document. An EMBEDDED document has no directory of its own, so
/// its includes resolve against `$BOUGH_HOME` — the only directory an installed binary owns.
fn include_base(origin: &profile::SourceOrigin) -> PathBuf {
    match origin {
        profile::SourceOrigin::File(p) => parent_dir(p),
        profile::SourceOrigin::Embedded(_) => bough_util::bough_home(),
    }
}

/// Stack every layer and produce the composition, together with the profile that selected them
/// (the launcher needs `invariants` and the profile name for `KernelOptions`).
///
/// `bough exec` composes TWICE: the first pass exists only to read the `exec` row's configured
/// defaults, so the synthetic layer can write the task without restating fields the bundle
/// already set (§0.5 replaces a config wholesale — there is no merge to lean on).
pub fn compose_plan(cli: &Cli, catalog: &Catalog) -> Result<(Profile, Composition), BootError> {
    let (profile, _sources, mut layers) = plan_layers(cli)?;

    if let Some(cmd) = &cli.command {
        // Composing TWICE is the point: the first pass exists only to read the row's configured
        // defaults, so the synthetic layer can write what the invocation named without restating
        // fields the bundle already set.
        let first = stack(catalog, &profile, &layers)?;
        match cmd {
            crate::cli::Command::Exec(args) => {
                let base = row_config(&first, crate::exec::EXEC_ROW);
                layers.push(crate::exec::exec_layer(args, &base));
            }
            crate::cli::Command::Mcp(args) => {
                let base = row_config(&first, MCP_CALL_ROW);
                layers.push(mcp_call_layer(args, &base));
            }
            crate::cli::Command::Wards(args) => {
                let base = row_config(&first, WARD_TEST_ROW);
                layers.push(wards_test_layer(args, &base));
            }
            // `restart`, `update` and `sync-mcp` are intercepted in `main` before anything
            // composes; the arms exist so the match stays exhaustive and write no layer if a
            // future path ever composes them.
            crate::cli::Command::Restart
            | crate::cli::Command::Update
            | crate::cli::Command::SyncMcp { .. } => {}
        }
    }

    let composition = stack(catalog, &profile, &layers)?;
    Ok((profile, composition))
}

/// The row `bough mcp call` writes. It lives in `bundles/bough-headless.yml`.
pub const MCP_CALL_ROW: &str = "mcp.call";
/// The row `bough wards test` writes. It lives in `bundles/bough-headless.yml`.
pub const WARD_TEST_ROW: &str = "wards.test";

/// A row's config as the bundles left it, or an empty mapping if no such row exists — in which
/// case composing will fail on the absent row id, loudly, which is the right failure.
fn row_config(c: &Composition, id: &str) -> serde_yaml::Value {
    find_row(&c.tree, &bough_kernel::EntryId::new(id))
        .map(|e| e.config.clone())
        .unwrap_or_else(|| serde_yaml::Value::Mapping(Default::default()))
}

/// Write `fields` onto `base`, per field. §0.5 replaces a config wholesale, so the unnamed fields
/// are carried over from the bundle here rather than restated on the command line.
fn write_fields(
    base: &serde_yaml::Value,
    fields: Vec<(&str, serde_yaml::Value)>,
) -> serde_yaml::Value {
    let mut config = match base {
        serde_yaml::Value::Mapping(m) => serde_yaml::Value::Mapping(m.clone()),
        _ => serde_yaml::Value::Mapping(Default::default()),
    };
    if let serde_yaml::Value::Mapping(map) = &mut config {
        for (k, v) in fields {
            map.insert(serde_yaml::Value::String(k.to_string()), v);
        }
    }
    config
}

/// One row's config as a whole patch layer.
fn one_row_layer(id: &str, layer: &str, config: serde_yaml::Value) -> Layer {
    let mut entries = std::collections::BTreeMap::new();
    entries.insert(
        bough_kernel::EntryId::new(id),
        bough_kernel::EntryPatch {
            config: Some(config),
            ..Default::default()
        },
    );
    Layer {
        id: LayerId::new(layer),
        base: bough_util::bough_home(),
        source: LayerSource::Synthetic(Patch {
            entries,
            insert: Vec::new(),
            remove: Vec::new(),
        }),
    }
}

/// The synthetic layer for one `bough mcp call`.
///
/// The JSON argument is written as DATA — a YAML string — for the same reason `bough exec`'s task
/// is: a shell-quoted `{...}` re-parsed as YAML would stop being the JSON the tool was handed.
pub fn mcp_call_layer(args: &crate::cli::McpArgs, base: &serde_yaml::Value) -> Layer {
    let crate::cli::McpCommand::Call {
        server,
        tool,
        args: json,
        print,
        keep_running,
    } = &args.command;
    let mut fields: Vec<(&str, serde_yaml::Value)> = vec![
        ("server", serde_yaml::Value::String(server.clone())),
        ("tool", serde_yaml::Value::String(tool.clone())),
        ("args", serde_yaml::Value::String(json.clone())),
        ("exit_when_done", serde_yaml::Value::Bool(!keep_running)),
    ];
    if let Some(p) = print {
        fields.push(("print", serde_yaml::Value::String(p.as_str().to_string())));
    }
    one_row_layer(MCP_CALL_ROW, MCP_CALL_LAYER, write_fields(base, fields))
}

/// The layer id, so a `--dump-config` reader can see where the call came from.
pub const MCP_CALL_LAYER: &str = "mcp-call";
/// See [`MCP_CALL_LAYER`].
pub const WARD_TEST_LAYER: &str = "wards-test";

/// The synthetic layer for one `bough wards test`.
pub fn wards_test_layer(args: &crate::cli::WardsArgs, base: &serde_yaml::Value) -> Layer {
    let crate::cli::WardsCommand::Test {
        file,
        since,
        print,
        keep_running,
    } = &args.command;
    let mut fields: Vec<(&str, serde_yaml::Value)> = vec![
        ("file", serde_yaml::Value::String(file.clone())),
        ("exit_when_done", serde_yaml::Value::Bool(!keep_running)),
    ];
    if let Some(s) = since {
        fields.push(("since", serde_yaml::Value::String(s.clone())));
    }
    if let Some(p) = print {
        fields.push(("print", serde_yaml::Value::String(p.as_str().to_string())));
    }
    one_row_layer(WARD_TEST_ROW, WARD_TEST_LAYER, write_fields(base, fields))
}

fn find_row<'a>(
    rows: &'a [bough_kernel::Entry],
    id: &bough_kernel::EntryId,
) -> Option<&'a bough_kernel::Entry> {
    for r in rows {
        if &r.id == id {
            return Some(r);
        }
        if let Some(hit) = find_row(&r.group, id) {
            return Some(hit);
        }
    }
    None
}

/// Stack a located plan into a composition. The ONE place layers become a tree.
fn stack(catalog: &Catalog, profile: &Profile, layers: &[Layer]) -> Result<Composition, BootError> {
    let mut composer = Composer::new(catalog, ExprEnv::new(&profile.name));
    for layer in layers {
        let patch = match &layer.source {
            LayerSource::Text { yaml, .. } => Patch::parse_in(yaml, &layer.base, &layer.id)?,
            LayerSource::Synthetic(p) => p.clone(),
            LayerSource::ProfilePatch => {
                // The profile document is deserialized whole, so its `patch:` block is grafted
                // here rather than at parse time.
                let mut p = profile.patch.clone();
                p.graft_includes(&layer.base, &layer.id)?;
                p
            }
        };
        composer.layer(layer.id.clone(), patch);
    }
    Ok(composer.compose()?)
}

/// The ONE composition path. `--dump-config` and boot both call it.
pub fn compose_for(cli: &Cli, catalog: &Catalog) -> Result<Composition, BootError> {
    compose_plan(cli, catalog).map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::profile::tests::Home;

    fn write_user_patch(home: &Home, yaml: &str) {
        std::fs::write(home.path().join("bough.patch.yml"), yaml).unwrap();
    }

    fn write_ui_patch(home: &Home, yaml: &str) {
        std::fs::write(home.path().join("bough.ui.patch.yml"), yaml).unwrap();
    }

    fn cli(profile: &str, patches: Vec<PathBuf>) -> Cli {
        Cli {
            profile: profile.to_string(),
            patches,
            dump_config: false,
            dump_format: crate::cli::DumpFormat::Yaml,
            check: true,
            no_watch: true,
            local: false,
            resident: false,
            shutdown_ms: 2000,
            root: None,
            command: None,
        }
    }

    fn ids(layers: &[Layer]) -> Vec<String> {
        layers.iter().map(|l| l.id.to_string()).collect()
    }

    fn write_mcp_patch(home: &Home, yaml: &str) {
        std::fs::write(home.path().join("bough.mcp.patch.yml"), yaml).unwrap();
    }

    #[test]
    fn layer_order_matches_requirements() {
        let home = Home::empty();
        write_user_patch(&home, "entries: {}\n");
        write_ui_patch(&home, "entries: {}\n");
        write_mcp_patch(&home, "entries: {}\n");
        let extra = home.path().join("extra.yml");
        std::fs::write(&extra, "entries: {}\n").unwrap();

        let (_p, _s, layers) = plan_layers(&cli("tui", vec![extra.clone()])).unwrap();
        assert_eq!(
            ids(&layers),
            vec![
                "bundle:bough-base".to_string(),
                "bundle:bough-tui-app".to_string(),
                "bundle:bough-codemode".to_string(),
                "profile:tui".to_string(),
                "mcp".to_string(),
                "user".to_string(),
                "ui".to_string(),
                format!("patch:0:{}", extra.display()),
            ],
            "sync-mcp's layer sits BELOW the user patch; the ui layer between user and --patch"
        );
    }

    #[test]
    fn user_patch_absent_is_not_an_error() {
        let _home = Home::empty(); // no bough.patch.yml written
        let (_p, _s, layers) = plan_layers(&cli("tui", vec![])).unwrap();
        assert_eq!(
            ids(&layers),
            vec![
                "bundle:bough-base",
                "bundle:bough-tui-app",
                "bundle:bough-codemode",
                "profile:tui"
            ],
            "an absent user patch is skipped silently, not an error"
        );
    }

    #[test]
    fn ui_patch_absent_is_not_an_error() {
        let home = Home::empty(); // no bough.ui.patch.yml written
        write_user_patch(&home, "entries: {}\n");
        let (_p, _s, layers) = plan_layers(&cli("tui", vec![])).unwrap();
        assert_eq!(
            ids(&layers),
            vec![
                "bundle:bough-base",
                "bundle:bough-tui-app",
                "bundle:bough-codemode",
                "profile:tui",
                "user"
            ],
            "an absent ui patch is skipped silently, not an error"
        );
    }

    #[test]
    fn cli_patches_apply_last_in_argument_order() {
        let home = Home::empty();
        write_user_patch(&home, "entries: {}\n");
        let a = home.path().join("a.yml");
        let b = home.path().join("b.yml");
        std::fs::write(&a, "entries: {}\n").unwrap();
        std::fs::write(&b, "entries: {}\n").unwrap();

        let (_p, _s, layers) = plan_layers(&cli("tui", vec![b.clone(), a.clone()])).unwrap();
        let got = ids(&layers);
        let tail = &got[got.len() - 3..];
        assert_eq!(
            tail,
            [
                "user".to_string(),
                format!("patch:0:{}", b.display()),
                format!("patch:1:{}", a.display()),
            ],
            "--patch layers come after the user patch, in argument order"
        );
    }

    fn cli_with(command: crate::cli::Command) -> Cli {
        let mut c = cli("tui", vec![]);
        c.command = Some(command);
        c
    }

    fn synthetic(layers: &[Layer]) -> &Patch {
        match &layers.last().expect("a synthetic layer").source {
            LayerSource::Synthetic(p) => p,
            other => panic!("the last layer must be synthetic, got {other:?}"),
        }
    }

    fn config_of(p: &Patch, id: &str) -> serde_yaml::Value {
        p.entries
            .get(&bough_kernel::EntryId::new(id))
            .unwrap_or_else(|| panic!("the patch writes `{id}`"))
            .config
            .clone()
            .expect("a config")
    }

    fn compose_with(command: crate::cli::Command) -> (Cli, Vec<Layer>) {
        let cli = cli_with(command);
        let catalog = Catalog::from_inventory().expect("the catalog");
        let (profile, _s, mut layers) = plan_layers(&cli).expect("the plan");
        let first = stack(&catalog, &profile, &layers).expect("the first pass composes");
        match &cli.command {
            Some(crate::cli::Command::Mcp(a)) => {
                layers.push(mcp_call_layer(a, &row_config(&first, MCP_CALL_ROW)))
            }
            Some(crate::cli::Command::Wards(a)) => {
                layers.push(wards_test_layer(a, &row_config(&first, WARD_TEST_ROW)))
            }
            other => panic!("this helper is for the two Phase-6 subcommands, got {other:?}"),
        }
        (cli, layers)
    }

    fn mcp_call(server: &str, tool: &str, args: &str) -> crate::cli::Command {
        crate::cli::Command::Mcp(crate::cli::McpArgs {
            command: crate::cli::McpCommand::Call {
                server: server.to_string(),
                tool: tool.to_string(),
                args: args.to_string(),
                print: None,
                keep_running: false,
            },
        })
    }

    fn wards_test(file: &str, since: Option<&str>) -> crate::cli::Command {
        crate::cli::Command::Wards(crate::cli::WardsArgs {
            command: crate::cli::WardsCommand::Test {
                file: file.to_string(),
                since: since.map(String::from),
                print: None,
                keep_running: false,
            },
        })
    }

    #[test]
    fn a_subcommand_selects_the_headless_profile_whatever_profile_said() {
        let cli = cli_with(mcp_call("fs", "read_file", "{}"));
        assert_eq!(cli.profile, "tui", "as typed");
        assert_eq!(
            cli.effective_profile(),
            crate::exec::EXEC_PROFILE,
            "as composed"
        );
        let (_p, _s, layers) = plan_layers(&cli).expect("the plan");
        let ids: Vec<String> = layers.iter().map(|l| l.id.to_string()).collect();
        assert!(
            ids.contains(&"bundle:bough-headless".to_string()),
            "the headless bundle is what carries the row: {ids:?}"
        );
    }

    #[test]
    fn mcp_call_writes_the_mcp_call_row_and_nothing_else() {
        let (_cli, layers) = compose_with(mcp_call("fs", "read_file", r#"{"path":"/tmp/x"}"#));
        let patch = synthetic(&layers);
        assert_eq!(patch.entries.len(), 1, "one row");
        assert!(patch.insert.is_empty() && patch.remove.is_empty());
        let cfg = config_of(patch, MCP_CALL_ROW);
        assert_eq!(cfg["server"].as_str().unwrap(), "fs");
        assert_eq!(cfg["tool"].as_str().unwrap(), "read_file");
        assert_eq!(
            cfg["args"].as_str().unwrap(),
            r#"{"path":"/tmp/x"}"#,
            "the JSON is DATA, never re-parsed as YAML"
        );
        assert_eq!(cfg["exit_when_done"], serde_yaml::Value::Bool(true));
        assert_eq!(
            cfg["print"].as_str().unwrap(),
            "text",
            "an unnamed field keeps the bundle's value"
        );
    }

    #[test]
    fn wards_test_writes_the_wards_test_row_and_nothing_else() {
        let (_cli, layers) = compose_with(wards_test("/tmp/w.rhai", Some("24h")));
        let patch = synthetic(&layers);
        assert_eq!(patch.entries.len(), 1);
        assert!(patch.insert.is_empty() && patch.remove.is_empty());
        let cfg = config_of(patch, WARD_TEST_ROW);
        assert_eq!(cfg["file"].as_str().unwrap(), "/tmp/w.rhai");
        assert_eq!(cfg["since"].as_str().unwrap(), "24h");
        assert_eq!(cfg["exit_when_done"], serde_yaml::Value::Bool(true));
    }

    #[test]
    fn an_unnamed_since_keeps_the_bundles_empty_default() {
        let (_cli, layers) = compose_with(wards_test("/tmp/w.rhai", None));
        let cfg = config_of(synthetic(&layers), WARD_TEST_ROW);
        assert_eq!(cfg["since"].as_str().unwrap(), "");
    }

    #[test]
    fn keep_running_is_the_negation_of_exit_when_done() {
        let cmd = crate::cli::Command::Mcp(crate::cli::McpArgs {
            command: crate::cli::McpCommand::Call {
                server: "fs".into(),
                tool: "t".into(),
                args: "{}".into(),
                print: Some(crate::cli::PrintFormat::Json),
                keep_running: true,
            },
        });
        let (_cli, layers) = compose_with(cmd);
        let cfg = config_of(synthetic(&layers), MCP_CALL_ROW);
        assert_eq!(cfg["exit_when_done"], serde_yaml::Value::Bool(false));
        assert_eq!(cfg["print"].as_str().unwrap(), "json");
    }

    #[test]
    fn a_subcommand_stops_the_patch_watch() {
        let mut cli = cli_with(wards_test("/tmp/w.rhai", None));
        cli.no_watch = false;
        cli.normalize();
        assert!(cli.no_watch);
    }
}
