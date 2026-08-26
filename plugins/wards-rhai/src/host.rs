//! Invariant: a ward file is a ROW. The host holds no ward logic at all — it holds a set of files,
//! and every file is one child entry whose config carries the file's digest. Hot reload is
//! therefore not a special path: it is a digest changing, which disposes EXACTLY that child and
//! mounts it again (P6-D11). A ward that fails to compile fails ITS OWN row and leaves its
//! siblings running.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One file's identity as the host tracks it: its path and the sha256 of its bytes.
pub type Digests = BTreeMap<PathBuf, String>;

/// What a rescan asks the host to do. PURE, so "exactly one child remounts" is a unit test and not
/// a race.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Added(PathBuf),
    Changed(PathBuf),
    Removed(PathBuf),
}

/// The difference between what is mounted and what is on disk, in a stable order.
pub fn plan_reload(mounted: &Digests, found: &Digests) -> Vec<Change> {
    let mut out = Vec::new();
    for (path, digest) in found {
        match mounted.get(path) {
            None => out.push(Change::Added(path.clone())),
            Some(old) if old != digest => out.push(Change::Changed(path.clone())),
            Some(_) => {}
        }
    }
    for path in mounted.keys() {
        if !found.contains_key(path) {
            out.push(Change::Removed(path.clone()));
        }
    }
    out
}

/// sha256 of some bytes, hex. The digest a child entry carries.
pub fn digest_of(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// Whether a file name matches the host's glob. Only the `*.ext` shape is supported, which is the
/// only shape the config ever holds; anything else is refused by `validate`.
pub fn matches(glob: &str, path: &Path) -> bool {
    match glob.strip_prefix('*') {
        Some(suffix) => path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(suffix)),
        None => path.file_name().and_then(|n| n.to_str()) == Some(glob),
    }
}

/// Every matching file in `dir`, with its digest. A missing directory is EMPTY, not an error: a
/// person with no wards yet is not a misconfiguration.
pub fn scan(dir: &Path, glob: &str) -> std::io::Result<Digests> {
    let mut out = Digests::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || !matches(glob, &path) {
            continue;
        }
        out.insert(path.clone(), digest_of(&std::fs::read(&path)?));
    }
    Ok(out)
}

/// The ward's name: the file stem. It is what `ward/fired`, the sender label and the reports say.
pub fn ward_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("ward")
        .to_string()
}

/// The child row's id, derived from the path so a remount reuses exactly one id.
pub fn child_id(path: &Path) -> String {
    format!("ward.{}", ward_name(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(pairs: &[(&str, &str)]) -> Digests {
        pairs
            .iter()
            .map(|(p, h)| (PathBuf::from(p), h.to_string()))
            .collect()
    }

    #[test]
    fn one_changed_file_plans_exactly_one_remount() {
        let plan = plan_reload(
            &d(&[("/w/a.rhai", "aaa"), ("/w/b.rhai", "bbb")]),
            &d(&[("/w/a.rhai", "aaa"), ("/w/b.rhai", "ccc")]),
        );
        assert_eq!(plan, vec![Change::Changed(PathBuf::from("/w/b.rhai"))]);
    }

    #[test]
    fn a_new_and_a_deleted_file_are_told_apart() {
        let plan = plan_reload(&d(&[("/w/a.rhai", "aaa")]), &d(&[("/w/b.rhai", "bbb")]));
        assert_eq!(
            plan,
            vec![
                Change::Added(PathBuf::from("/w/b.rhai")),
                Change::Removed(PathBuf::from("/w/a.rhai")),
            ]
        );
    }

    #[test]
    fn an_unchanged_tree_plans_nothing() {
        let one = d(&[("/w/a.rhai", "aaa")]);
        assert!(plan_reload(&one, &one).is_empty());
    }

    #[test]
    fn the_glob_matches_by_extension_only() {
        assert!(matches("*.rhai", Path::new("/w/reviews.rhai")));
        assert!(!matches("*.rhai", Path::new("/w/reviews.rhai.bak")));
    }
}
