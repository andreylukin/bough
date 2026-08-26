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
//!   → ~/.bough/bough.patch.yml (absent ⇒ skipped silently)                   "user"
//!   → each --patch FILE, in argument order                                   "patch:<n>:<file>"
//! ```

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
}

/// The plan of layers, in the normative order, plus the profile that named them.
///
/// `--dump-config` and boot both go through here; nothing else stacks layers.
pub fn plan_layers(cli: &Cli) -> Result<(Profile, Sources, Vec<Layer>), BootError> {
    let root = cli.root.as_deref();
    let (profile, sources) = profile::resolve_profile(&cli.profile, root)?;

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
pub fn compose_plan(cli: &Cli, catalog: &Catalog) -> Result<(Profile, Composition), BootError> {
    let (profile, _sources, layers) = plan_layers(cli)?;

    let mut composer = Composer::new(catalog, ExprEnv::new(&profile.name));
    for layer in layers {
        let patch = match &layer.source {
            LayerSource::Text { yaml, .. } => Patch::parse_in(yaml, &layer.base, &layer.id)?,
            LayerSource::ProfilePatch => {
                // The profile document is deserialized whole, so its `patch:` block is grafted
                // here rather than at parse time.
                let mut p = profile.patch.clone();
                p.graft_includes(&layer.base, &layer.id)?;
                p
            }
        };
        composer.layer(layer.id, patch);
    }
    let composition = composer.compose()?;
    Ok((profile, composition))
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

    fn cli(profile: &str, patches: Vec<PathBuf>) -> Cli {
        Cli {
            profile: profile.to_string(),
            patches,
            dump_config: false,
            dump_format: crate::cli::DumpFormat::Yaml,
            check: true,
            no_watch: true,
            root: None,
        }
    }

    fn ids(layers: &[Layer]) -> Vec<String> {
        layers.iter().map(|l| l.id.to_string()).collect()
    }

    #[test]
    fn layer_order_matches_requirements() {
        let home = Home::empty();
        write_user_patch(&home, "entries: {}\n");
        let extra = home.path().join("extra.yml");
        std::fs::write(&extra, "entries: {}\n").unwrap();

        let (_p, _s, layers) = plan_layers(&cli("tui", vec![extra.clone()])).unwrap();
        assert_eq!(
            ids(&layers),
            vec![
                "bundle:bough-base".to_string(),
                "bundle:bough-tui-app".to_string(),
                "profile:tui".to_string(),
                "user".to_string(),
                format!("patch:0:{}", extra.display()),
            ]
        );
    }

    #[test]
    fn user_patch_absent_is_not_an_error() {
        let _home = Home::empty(); // no bough.patch.yml written
        let (_p, _s, layers) = plan_layers(&cli("tui", vec![])).unwrap();
        assert_eq!(
            ids(&layers),
            vec!["bundle:bough-base", "bundle:bough-tui-app", "profile:tui"],
            "an absent user patch is skipped silently, not an error"
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
}
