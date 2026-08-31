//! Invariant: an installed binary must boot with no repo checked out. Profiles and bundles are
//! embedded with `include_dir!` and overridable, in order, by `--root DIR` and then
//! `$BOUGH_HOME/{profiles,bundles}` (Decision D11).
//!
//! Known trap from the old tree: `include_dir`'s `files()` is NOT recursive — use `find()` or
//! `get_file()` with explicit paths.

use std::path::{Path, PathBuf};

use bough_kernel::Patch;

use crate::cli::BootError;

/// The embedded copies. Present so `bough` works from an installed binary.
pub static EMBEDDED_PROFILES: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../profiles");
/// See [`EMBEDDED_PROFILES`].
pub static EMBEDDED_BUNDLES: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../bundles");

/// One profile document.
#[derive(Debug, serde::Deserialize)]
pub struct Profile {
    pub name: String,
    /// Bundles to stack, in this order.
    pub bundles: Vec<String>,
    /// Create the kernel's invariant runner (`dev` only in Phase 0).
    #[serde(default)]
    pub invariants: bool,
    /// The profile's own patch layer, stacked after every bundle.
    #[serde(default)]
    pub patch: Patch,
}

/// Where each document actually came from, so an error can name the search path and
/// `--dump-config` can be honest about provenance.
#[derive(Debug, Clone)]
pub struct Sources {
    /// Directories searched, in order.
    pub searched: Vec<PathBuf>,
    /// Resolved profile document.
    pub profile: SourceOrigin,
    /// Resolved bundle documents, in profile order.
    pub bundles: Vec<(String, SourceOrigin)>,
}

/// Where one document was read from.
#[derive(Debug, Clone)]
pub enum SourceOrigin {
    File(PathBuf),
    Embedded(&'static str),
}

impl std::fmt::Display for SourceOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceOrigin::File(p) => write!(f, "{}", p.display()),
            SourceOrigin::Embedded(n) => write!(f, "<embedded>/{n}"),
        }
    }
}

/// Which embedded directory a lookup goes to. The two kinds live in sibling directories under
/// every search root, so one function serves both.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Profiles,
    Bundles,
}

impl Kind {
    fn dir(self) -> &'static str {
        match self {
            Kind::Profiles => "profiles",
            Kind::Bundles => "bundles",
        }
    }
    fn embedded(self) -> &'static include_dir::Dir<'static> {
        match self {
            Kind::Profiles => &EMBEDDED_PROFILES,
            Kind::Bundles => &EMBEDDED_BUNDLES,
        }
    }
}

/// The directories searched for `profiles/` and `bundles/`, in order: `--root`, then
/// `$BOUGH_HOME`. The embedded copies are the last resort and are not a directory.
pub fn search_roots(root: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(r) = root {
        v.push(r.to_path_buf());
    }
    v.push(bough_util::bough_home());
    v
}

/// Every place a document of `kind` named `name` could have been, for an error message.
fn candidates(kind: Kind, name: &str, root: Option<&Path>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = search_roots(root)
        .into_iter()
        .map(|r| r.join(kind.dir()).join(format!("{name}.yml")))
        .collect();
    v.push(PathBuf::from(format!(
        "<embedded>/{}/{name}.yml",
        kind.dir()
    )));
    v
}

/// Read one document as text, honouring the search order.
fn read_doc(
    kind: Kind,
    name: &str,
    root: Option<&Path>,
) -> Result<Option<(String, SourceOrigin)>, BootError> {
    for r in search_roots(root) {
        let p = r.join(kind.dir()).join(format!("{name}.yml"));
        if p.is_file() {
            let text = std::fs::read_to_string(&p).map_err(|e| BootError::BadFile {
                path: p.clone(),
                detail: e.to_string(),
            })?;
            return Ok(Some((text, SourceOrigin::File(p))));
        }
    }
    // `include_dir`'s `files()` is not recursive; `get_file` with an explicit path is.
    let rel = format!("{name}.yml");
    if let Some(f) = kind.embedded().get_file(&rel) {
        let text = f
            .contents_utf8()
            .ok_or_else(|| BootError::BadFile {
                path: PathBuf::from(&rel),
                detail: "embedded document is not UTF-8".into(),
            })?
            .to_string();
        // Leak-free: the embedded name is already `'static` in the include_dir tree.
        let leaked: &'static str = f.path().to_str().unwrap_or("?");
        return Ok(Some((text, SourceOrigin::Embedded(leaked))));
    }
    Ok(None)
}

/// Resolve a profile and locate the bundles it names.
///
/// Locating, not parsing: bundle documents become patch layers in [`crate::compose`], which is the
/// one place layers are stacked.
pub fn resolve_profile(name: &str, root: Option<&Path>) -> Result<(Profile, Sources), BootError> {
    let searched = candidates(Kind::Profiles, name, root);
    let (text, origin) =
        read_doc(Kind::Profiles, name, root)?.ok_or_else(|| BootError::UnknownProfile {
            name: name.to_string(),
            searched: searched.clone(),
        })?;
    let profile: Profile = serde_yaml::from_str(&text).map_err(|e| BootError::BadFile {
        path: PathBuf::from(origin.to_string()),
        detail: e.to_string(),
    })?;

    let mut bundles = Vec::new();
    for b in &profile.bundles {
        let (_, o) = locate_bundle(b, &profile.name, root)?;
        bundles.push((b.clone(), o));
    }

    Ok((
        profile,
        Sources {
            searched,
            profile: origin,
            bundles,
        },
    ))
}

/// Find one bundle document and return its text.
fn locate_bundle(
    name: &str,
    profile: &str,
    root: Option<&Path>,
) -> Result<(String, SourceOrigin), BootError> {
    read_doc(Kind::Bundles, name, root)?.ok_or_else(|| BootError::UnknownBundle {
        name: name.to_string(),
        profile: profile.to_string(),
        searched: candidates(Kind::Bundles, name, root),
    })
}

/// Read one bundle document as raw text plus its origin.
pub fn load_bundle_text(
    name: &str,
    profile: &str,
    root: Option<&Path>,
) -> Result<(String, SourceOrigin), BootError> {
    locate_bundle(name, profile, root)
}

/// Read one bundle document as a patch layer.
pub fn load_bundle(name: &str, root: Option<&Path>) -> Result<(Patch, SourceOrigin), BootError> {
    let (text, origin) = locate_bundle(name, "", root)?;
    let patch = Patch::parse(&text).map_err(|e| BootError::BadFile {
        path: PathBuf::from(origin.to_string()),
        detail: e.to_string(),
    })?;
    Ok((patch, origin))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// `BOUGH_HOME` is process-global; every test in this crate that sets it serialises on THIS
    /// lock — one lock for the whole crate, or two modules quietly reset each other's home.
    pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static L: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        L.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) struct Home {
        pub(crate) dir: tempfile::TempDir,
        prev: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Home {
        pub(crate) fn empty() -> Self {
            let lock = env_lock();
            let dir = tempfile::tempdir().unwrap();
            let prev = std::env::var_os("BOUGH_HOME");
            std::env::set_var("BOUGH_HOME", dir.path());
            Self {
                dir,
                prev,
                _lock: lock,
            }
        }
        pub(crate) fn path(&self) -> &Path {
            self.dir.path()
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("BOUGH_HOME", v),
                None => std::env::remove_var("BOUGH_HOME"),
            }
        }
    }

    #[test]
    fn resolve_embedded_profile() {
        let _h = Home::empty();
        let (p, s) = resolve_profile("tui", None).expect("tui resolves from the embedded copies");
        assert_eq!(p.name, "tui");
        assert_eq!(
            p.bundles,
            vec!["bough-base", "bough-tui-app", "bough-codemode"]
        );
        assert!(!p.invariants);
        assert!(matches!(s.profile, SourceOrigin::Embedded(_)));
        assert_eq!(s.bundles.len(), 3);
        assert!(s
            .bundles
            .iter()
            .all(|(_, o)| matches!(o, SourceOrigin::Embedded(_))));
    }

    #[test]
    fn root_overrides_embedded() {
        let _h = Home::empty();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("profiles")).unwrap();
        std::fs::create_dir_all(root.path().join("bundles")).unwrap();
        std::fs::write(
            root.path().join("profiles/tui.yml"),
            "name: tui\nbundles: [only-mine]\ninvariants: true\n",
        )
        .unwrap();
        std::fs::write(root.path().join("bundles/only-mine.yml"), "[]\n").unwrap();

        let (p, s) = resolve_profile("tui", Some(root.path())).unwrap();
        assert_eq!(p.bundles, vec!["only-mine"]);
        assert!(
            p.invariants,
            "the root copy, not the embedded one, was read"
        );
        match &s.profile {
            SourceOrigin::File(f) => assert!(f.starts_with(root.path())),
            other => panic!("expected a file origin, got {other:?}"),
        }
    }

    #[test]
    fn unknown_profile_names_the_search_path() {
        let _h = Home::empty();
        let root = tempfile::tempdir().unwrap();
        let err = resolve_profile("nope", Some(root.path())).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nope"), "{msg}");
        assert!(
            msg.contains(&root.path().join("profiles").display().to_string()),
            "the error must name the --root search path: {msg}"
        );
        assert!(
            msg.contains("<embedded>/profiles"),
            "the error must name the embedded fallback: {msg}"
        );
    }
}
