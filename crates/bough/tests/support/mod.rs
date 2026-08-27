//! Shared harness for the launcher's Phase 0 integration tests.
//!
//! Invariant this file holds: these tests boot and recompose through the LAUNCHER'S OWN code —
//! `bough::compose::compose_plan` and `bough::watch::recompose_once` — over a real `$BOUGH_HOME`
//! with a real `profiles/` and `bundles/` on disk. It reproduces nothing. That is the point: the
//! previous harness stacked its own two layers and emitted `config-update-failed` itself, so the
//! SWAP gate never exercised the normative §0.5 layer stack and V7's broadcast half was
//! self-fulfilling.
//!
//! `$BOUGH_HOME` is process-global, so every test using [`boot_with`] MUST hold
//! `bough_plugin_hello::trace::test_lock()` for its whole body. They all do.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bough::cli::{BootError, Cli, DumpFormat};
use bough_kernel::{
    Catalog, ComposeError, Composition, Kernel, KernelError, KernelOptions, RowSnapshot,
};

/// The bundle name the harness writes its test tree to.
const TEST_BUNDLE: &str = "phase0-test";

/// A throwaway `$BOUGH_HOME`, with the `Cli` the launcher was booted with. Removed on drop.
///
/// Hand-rolled rather than `tempfile` so this package's manifest needs no new dev-dependency.
pub struct TempDir {
    path: PathBuf,
    cli: Option<Arc<Cli>>,
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("bough-phase0-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a temp dir");
        TempDir { path, cli: None }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Where the launcher looks for the user patch layer.
    pub fn patch_path(&self) -> PathBuf {
        self.path.join("bough.patch.yml")
    }
    /// The `Cli` this home was booted with — the one the launcher's own recompose path takes.
    pub fn cli(&self) -> &Arc<Cli> {
        self.cli.as_ref().expect("this home was not booted")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Write (or overwrite) `$BOUGH_HOME/bough.patch.yml`.
pub fn write_patch(dir: &TempDir, yaml: &str) {
    std::fs::write(dir.patch_path(), yaml).expect("the patch file is writable");
}

/// Remove the user patch layer.
pub fn clear_patch(dir: &TempDir) {
    let _ = std::fs::remove_file(dir.patch_path());
}

/// The repo's `profiles/` and `bundles/`, as the launcher's `--root` would see them.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repo root exists")
}

/// The subset of a profile document these tests need.
#[derive(serde::Deserialize)]
struct ProfileDoc {
    #[serde(default)]
    invariants: bool,
}

/// Whether the named profile turns the kernel's invariant runner on.
pub fn profile_runs_invariants(profile: &str) -> bool {
    let path = repo_root().join("profiles").join(format!("{profile}.yml"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let doc: ProfileDoc = serde_yaml::from_str(&text).expect("the profile document parses");
    doc.invariants
}

/// Lay a `$BOUGH_HOME` out the way an installed bough sees one: the repo's real profile document
/// with its bundle list pointed at this test's tree, and the tree itself as a bundle file.
///
/// `--root` is deliberately NOT used: the launcher searches `$BOUGH_HOME/{profiles,bundles}` first
/// (Decision D11), which is the override path a user has.
fn lay_out_home(dir: &TempDir, bundle_yaml: &str, profile: &str) {
    std::fs::create_dir_all(dir.path().join("bundles")).unwrap();
    std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
    std::fs::write(
        dir.path()
            .join("bundles")
            .join(format!("{TEST_BUNDLE}.yml")),
        bundle_yaml,
    )
    .unwrap();

    // Start from the repo's real profile so `invariants` and every other field stay honest; only
    // the bundle list is swapped for the test tree.
    let src = repo_root().join("profiles").join(format!("{profile}.yml"));
    let text = std::fs::read_to_string(&src).unwrap_or_else(|e| panic!("{}: {e}", src.display()));
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("the profile parses");
    doc.as_mapping_mut()
        .expect("a profile is a mapping")
        .insert(
            serde_yaml::Value::from("bundles"),
            serde_yaml::Value::from(vec![serde_yaml::Value::from(TEST_BUNDLE)]),
        );
    std::fs::write(
        dir.path().join("profiles").join(format!("{profile}.yml")),
        serde_yaml::to_string(&doc).unwrap(),
    )
    .unwrap();
}

fn cli_for(profile: &str) -> Cli {
    Cli {
        profile: profile.to_string(),
        patches: Vec::new(),
        dump_config: false,
        dump_format: DumpFormat::Yaml,
        check: false,
        // The tests drive `recompose_once` directly rather than waiting on a file watcher.
        no_watch: true,
        shutdown_ms: 2000,
        root: None,
        command: None,
    }
}

/// Compose through the launcher's own layer stack (§0.5): bundles → profile patch → user patch →
/// `--patch`.
pub fn compose_layers(
    catalog: &Catalog,
    _bundle_yaml: &str,
    dir: &TempDir,
) -> Result<Composition, ComposeError> {
    match bough::compose::compose_for(dir.cli(), catalog) {
        Ok(c) => Ok(c),
        Err(BootError::Compose(c)) => Err(c),
        Err(other) => panic!("unexpected launcher failure: {other}"),
    }
}

/// Boot `bundle_yaml` under the `tui` profile with a fresh `$BOUGH_HOME`.
pub async fn boot_with(bundle_yaml: &str) -> (Arc<Kernel>, TempDir) {
    boot_with_profile(bundle_yaml, "tui").await
}

/// Boot `bundle_yaml` under a named profile, through the launcher's composition path.
///
/// Sets `$BOUGH_HOME`; the caller must hold `trace::test_lock()`.
pub async fn boot_with_profile(bundle_yaml: &str, profile: &str) -> (Arc<Kernel>, TempDir) {
    let mut dir = TempDir::new(profile);
    // SAFETY: single-threaded within the test, which holds the fixture's process-wide test lock.
    unsafe { std::env::set_var("BOUGH_HOME", dir.path()) };
    lay_out_home(&dir, bundle_yaml, profile);
    dir.cli = Some(Arc::new(cli_for(profile)));

    let catalog = Catalog::from_inventory().expect("the linked catalog has no duplicate names");
    let (resolved, composition) =
        bough::compose::compose_plan(dir.cli(), &catalog).expect("the tree composes");
    assert_eq!(resolved.name, profile);
    assert_eq!(
        resolved.invariants,
        profile_runs_invariants(profile),
        "the harness must not change what the profile decides"
    );

    let kernel = Kernel::new(
        catalog,
        KernelOptions {
            profile: resolved.name.clone(),
            invariants: resolved.invariants,
        },
    );
    kernel.load(composition).await.expect("the tree mounts");
    assert!(kernel.quiesce().await, "the booted tree must quiesce");
    (kernel, dir)
}

/// Recompose from the layers on disk — the launcher's LIVE path (`bough::watch::recompose_once`),
/// the same function the patch-file watch task calls.
///
/// Nothing is emitted here: `config-update-failed` is broadcast by production code, from inside
/// the kernel for a candidate that fails to mount and from `recompose_once` for one that fails to
/// compose.
pub async fn recompose(
    kernel: &Kernel,
    _bundle_yaml: &str,
    dir: &TempDir,
) -> Result<(), ComposeError> {
    let outcome = bough::watch::recompose_once(kernel, dir.cli()).await;
    kernel.quiesce().await;
    match outcome {
        Ok(()) => Ok(()),
        Err(BootError::ComposeShared(c)) => Err(rebuild(&c)),
        Err(BootError::Compose(c)) => Err(c),
        Err(BootError::Kernel(KernelError::Compose(c))) => Err(c),
        Err(other) => panic!("unexpected launcher failure: {other}"),
    }
}

/// `ComposeError` is not `Clone` and the broadcast payload is shared behind an `Arc`, so the
/// variant a test matches on is reconstructed from the shared one. Only the shapes the Phase 0
/// tests assert on are reconstructed; anything else keeps its rendered message.
fn rebuild(e: &ComposeError) -> ComposeError {
    match e {
        ComposeError::UnknownPlugin {
            entry,
            plugin,
            layer,
        } => ComposeError::UnknownPlugin {
            entry: entry.clone(),
            plugin: plugin.clone(),
            layer: layer.clone(),
        },
        ComposeError::BadConfig {
            entry,
            plugin,
            layer,
            source,
        } => ComposeError::BadConfig {
            entry: entry.clone(),
            plugin: plugin.clone(),
            layer: layer.clone(),
            source: bough_kernel::ConfigError::Rejected {
                detail: source.to_string(),
            },
        },
        other => ComposeError::BadYaml {
            layer: bough_kernel::LayerId::new("user"),
            detail: other.to_string(),
        },
    }
}

/// One row from the kernel's structural snapshot, searched depth-first.
pub fn row(kernel: &Kernel, id: &str) -> RowSnapshot {
    maybe_row(kernel, id).unwrap_or_else(|| panic!("no row `{id}` in the tree"))
}

/// As [`row`], but absence is an answer.
pub fn maybe_row(kernel: &Kernel, id: &str) -> Option<RowSnapshot> {
    fn find(rows: &[RowSnapshot], id: &str) -> Option<RowSnapshot> {
        for r in rows {
            if r.id.as_str() == id {
                return Some(r.clone());
            }
            if let Some(found) = find(&r.children, id) {
                return Some(found);
            }
        }
        None
    }
    find(&kernel.snapshot().rows, id)
}

/// The Phase 0 base composition, as `bundles/bough-base.yml` ships it.
pub const BASE: &str = "\
- id: greeting.provider
  plugin: greeting-echo
  config: { suffix: \"\" }
- id: hello.greeter
  plugin: hello
  config:
    who: world
    log_level: info
";

// ---------------------------------------------------------------------------
// Phase 2: booting the REAL bundles
// ---------------------------------------------------------------------------

/// A fixture patch file, by base name, under `crates/bough/tests/fixtures/`.
pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Boot the SHIPPED `profiles/` + `bundles/` — not a test tree — with extra `--patch` layers.
///
/// Phase 0's `boot_with` swaps the bundle list for a fixture, which is right for testing the
/// loader; Phase 2's gates are about the rows Andrey actually ships, so these boot the real files
/// through `--root` with a throwaway `$BOUGH_HOME` for the ledger and the user patch layer.
pub async fn boot_real(profile: &str, patches: &[PathBuf]) -> (Arc<Kernel>, TempDir) {
    // The invariant recorders are PER PROCESS and keyed by `FiberUid`, and a fresh `Kernel` mints
    // fiber uids from zero — so a previous test in this binary can leave observations that the
    // next one's runner reads as its own. Every boot starts from a clean stream; a test whose
    // whole point is the recorded stream must be the only writer of it.
    bough_plugin_ledger::invariant::clear();
    bough_plugin_agents::invariant::clear();
    bough_plugin_commands::invariant::clear();

    let mut dir = TempDir::new(&format!("phase2-{profile}"));
    // SAFETY: the caller holds the fixture's process-wide test lock.
    unsafe { std::env::set_var("BOUGH_HOME", dir.path()) };
    // AND `$HOME`. The shipped `old-feed` row defaults `jungler_db`/`bough_db` to
    // `!!expr home_path(..)`, which resolves against `$HOME` and NOT `$BOUGH_HOME` — so a
    // `cargo test` that boots the shipped tui bundle used to activate the bridge against the
    // developer's REAL `~/.bough/bough.db` and `~/.jungler/jungler.db` and, on a machine that has
    // one, import live events as mail into the test ledger. Tests are hermetic (AGENTS.md).
    // SAFETY: as above.
    unsafe { std::env::set_var("HOME", dir.path()) };
    let mut cli = cli_for(profile);
    cli.root = Some(repo_root());
    cli.patches = patches.to_vec();
    dir.cli = Some(Arc::new(cli));

    let catalog = Catalog::from_inventory().expect("the linked catalog has no duplicate names");
    let (resolved, composition) =
        bough::compose::compose_plan(dir.cli(), &catalog).expect("the shipped tree composes");
    let kernel = Kernel::new(
        catalog,
        KernelOptions {
            profile: resolved.name.clone(),
            invariants: true,
        },
    );
    kernel
        .load(composition)
        .await
        .expect("the shipped tree mounts");
    assert!(kernel.quiesce().await, "the shipped tree must quiesce");
    assert!(
        kernel.snapshot().unresolved().is_empty(),
        "an enabled row that never activates is a boot failure (§0.2): {:#?}",
        kernel.snapshot().unresolved()
    );
    (kernel, dir)
}

/// A row's `Context`, for a test that needs to reach a service the way a plugin does.
///
/// `exec` is the row of choice: it injects both `agents` and `ledger`, so its context resolves
/// exactly the two keys these gates read, and nothing else.
pub fn row_ctx(kernel: &Kernel, id: &str) -> bough_kernel::Context {
    kernel
        .row_context(&bough_kernel::EntryId::new(id))
        .unwrap_or_else(|| panic!("row `{id}` has no context"))
}
