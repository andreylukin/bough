//! Shared harness for the launcher's Phase 0 integration tests.
//!
//! Invariant this file holds: the tests below compose through the SAME layer order the launcher
//! does (§0.5) — bundle layer, then `$BOUGH_HOME/bough.patch.yml` — so a patch that swaps a row in
//! a test swaps it the same way a user's patch file would. The launcher's own module is private to
//! its binary target, so this harness reproduces the layer stack rather than importing it; the
//! `--dump-config` tests drive the real binary instead, which is what keeps the two honest.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{
    Catalog, ComposeError, Composer, Composition, ExprEnv, Kernel, KernelOptions, LayerId, Patch,
    RowSnapshot,
};

/// A throwaway `$BOUGH_HOME`. Removed on drop.
///
/// Hand-rolled rather than `tempfile` so this package's manifest — WP-5's file — needs no new
/// dev-dependency.
pub struct TempDir(PathBuf);

static COUNTER: AtomicU64 = AtomicU64::new(0);

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("bough-phase0-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a temp dir");
        TempDir(path)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
    /// Where the launcher looks for the user patch layer.
    pub fn patch_path(&self) -> PathBuf {
        self.0.join("bough.patch.yml")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
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

/// The subset of a profile document these tests need. The launcher's own `Profile` lives in a
/// private module of its binary target, so it cannot be imported here.
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

/// Stack the bundle layer and, if it exists, the user patch layer — the launcher's order (§0.5).
pub fn compose_layers(
    catalog: &Catalog,
    bundle_yaml: &str,
    dir: &TempDir,
) -> Result<Composition, ComposeError> {
    let bundle: Patch = serde_yaml::from_str(bundle_yaml).expect("the bundle document parses");
    let mut composer = Composer::new(catalog, ExprEnv::new("test"));
    composer.layer(LayerId::new("bundle:test"), bundle);
    if let Ok(text) = std::fs::read_to_string(dir.patch_path()) {
        let user: Patch = serde_yaml::from_str(&text).expect("the user patch document parses");
        composer.layer(LayerId::new("user"), user);
    }
    composer.compose()
}

/// Boot `bundle_yaml` under the `tui` profile with a fresh `$BOUGH_HOME`.
pub async fn boot_with(bundle_yaml: &str) -> (Arc<Kernel>, TempDir) {
    boot_with_profile(bundle_yaml, "tui").await
}

/// Boot `bundle_yaml` under a named profile: the profile decides whether the kernel's invariant
/// runner exists at all (§2.9).
pub async fn boot_with_profile(bundle_yaml: &str, profile: &str) -> (Arc<Kernel>, TempDir) {
    let dir = TempDir::new(profile);
    let catalog = Catalog::from_inventory().expect("the linked catalog has no duplicate names");
    let composition = compose_layers(&catalog, bundle_yaml, &dir).expect("the tree composes");
    let kernel = Kernel::new(
        catalog,
        KernelOptions {
            profile: profile.to_string(),
            invariants: profile_runs_invariants(profile),
            reconcile_debounce: Duration::from_millis(0),
        },
    );
    kernel.load(composition).await.expect("the tree mounts");
    kernel.quiesce().await;
    (kernel, dir)
}

/// Recompose from the layers on disk and hand the candidate to the kernel — the launcher's watch
/// path (§2.13), in-process.
///
/// A candidate that fails to compose never reaches `Kernel::update`, so the broadcast of
/// `config-update-failed` is issued here, exactly where `crates/bough/src/watch.rs` issues it. The
/// claim the tests then check is the kernel's: the last good tree keeps running.
pub async fn recompose(
    kernel: &Kernel,
    bundle_yaml: &str,
    dir: &TempDir,
) -> Result<(), ComposeError> {
    let catalog = Catalog::from_inventory().expect("catalog");
    match compose_layers(&catalog, bundle_yaml, dir) {
        Ok(candidate) => {
            kernel.update(candidate).await.expect("the tree updates");
            kernel.quiesce().await;
            Ok(())
        }
        Err(e) => {
            kernel
                .root()
                .emit::<bough_kernel::event::ConfigUpdateFailed>(Arc::new(clone_error(&e)));
            kernel.quiesce().await;
            Err(e)
        }
    }
}

/// `ComposeError` is not `Clone` (it carries `#[source]` chains), and the broadcast payload is an
/// `Arc<ComposeError>`, so the emitted copy is rebuilt from the rendered message.
fn clone_error(e: &ComposeError) -> ComposeError {
    ComposeError::BadYaml {
        layer: LayerId::new("user"),
        detail: e.to_string(),
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
