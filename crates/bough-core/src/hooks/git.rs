//! Cloning and updating hook repositories.
//!
//! Through the `git` BINARY, not a library: git is already a hard requirement
//! of this harness, the user's credential helpers and SSH config already work,
//! and a private repo they can clone by hand is a repo bough can clone. A
//! vendored implementation would have its own idea about all three.
//!
//! NOTHING HERE RUNS ON ITS OWN. `add` clones once, `update` re-fetches when
//! you ask, and both print the commit they landed on. A harness that pulled
//! new code between turns would be a supply chain with no gate on it — the
//! whole point of recording a `sha` is that you can see when the code you are
//! running changed.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::sources::{
    read_sources_file, repos_dir, sources_path, write_sources_file, GitSource, SourcesFile,
};

/// What one `add` or `update` did, for the CLI to print.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landed {
    pub slug: String,
    pub repo: String,
    /// The commit now checked out.
    pub sha: String,
    /// The commit before, when this was an update over an existing clone.
    pub was: Option<String>,
    /// How many `.lua` files it contributes.
    pub hooks: usize,
}

impl Landed {
    pub fn changed(&self) -> bool {
        self.was.as_deref() != Some(self.sha.as_str())
    }
}

fn git(dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    let out = cmd
        .args(args)
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn head_sha(dir: &Path) -> Option<String> {
    git(Some(dir), &["rev-parse", "HEAD"]).ok()
}

fn lua_count(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "lua"))
        .count()
}

/// Clone `repo` and record it in `hooks.json`.
///
/// The hooks it brings are OFF. Cloning is consent to read someone's code, not
/// consent to run it — `hooks::sources` carries that argument, and this is the
/// call site that depends on it.
pub fn add(repo: &str, rev: Option<&str>, dir: Option<&str>) -> Result<Landed, String> {
    let source = GitSource {
        repo: repo.to_string(),
        rev: rev.map(String::from),
        dir: dir.map(String::from),
        sha: None,
    };
    let slug = source.slug();
    if slug.is_empty() {
        return Err(format!("{repo} does not name a repository"));
    }
    let mut file = read_sources_file(&sources_path());
    if file.sources.iter().any(|s| s.slug() == slug) {
        return Err(format!(
            "{slug} is already a source — `bough hooks update {slug}` re-fetches it."
        ));
    }
    let target = repos_dir().join(&slug);
    if target.exists() {
        std::fs::remove_dir_all(&target).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(repos_dir()).map_err(|e| e.to_string())?;
    git(
        None,
        &["clone", "--depth", "1", repo, &target.to_string_lossy()],
    )?;
    if let Some(rev) = rev {
        // A shallow clone has one commit, so a named rev needs fetching before
        // it can be checked out.
        git(Some(&target), &["fetch", "--depth", "1", "origin", rev])?;
        git(Some(&target), &["checkout", "FETCH_HEAD"])?;
    }
    let sha = head_sha(&target).unwrap_or_default();
    let hooks_at = match dir {
        Some(sub) => target.join(sub),
        None => target.clone(),
    };
    let mut recorded = source.clone();
    recorded.sha = Some(sha.clone());
    file.sources.push(recorded);
    write_sources_file(&sources_path(), &file).map_err(|e| e.to_string())?;
    Ok(Landed {
        slug,
        repo: repo.to_string(),
        sha,
        was: None,
        hooks: lua_count(&hooks_at),
    })
}

/// Re-fetch one source, or every source when `slug` is `None`.
pub fn update(slug: Option<&str>) -> Result<Vec<Landed>, String> {
    let mut file = read_sources_file(&sources_path());
    if file.sources.is_empty() {
        return Ok(Vec::new());
    }
    let mut landed = Vec::new();
    for source in file.sources.iter_mut() {
        let this = source.slug();
        if slug.is_some_and(|want| want != this) {
            continue;
        }
        let target = repos_dir().join(&this);
        if !target.exists() {
            // Recorded but never cloned, or the clone was deleted by hand.
            // Re-clone rather than fail: the sources file is the intent.
            git(
                None,
                &[
                    "clone",
                    "--depth",
                    "1",
                    &source.repo,
                    &target.to_string_lossy(),
                ],
            )?;
        }
        let was = head_sha(&target);
        let rev = source.rev.clone().unwrap_or_else(|| "HEAD".to_string());
        git(Some(&target), &["fetch", "--depth", "1", "origin", &rev])?;
        git(Some(&target), &["checkout", "FETCH_HEAD"])?;
        let sha = head_sha(&target).unwrap_or_default();
        source.sha = Some(sha.clone());
        let hooks_at = match &source.dir {
            Some(sub) => target.join(sub),
            None => target.clone(),
        };
        landed.push(Landed {
            slug: this,
            repo: source.repo.clone(),
            sha,
            was,
            hooks: lua_count(&hooks_at),
        });
    }
    if slug.is_some() && landed.is_empty() {
        return Err(format!(
            "no source {} — `bough hooks list` names the ones you have.",
            slug.unwrap_or_default()
        ));
    }
    write_sources_file(&sources_path(), &file).map_err(|e| e.to_string())?;
    super::reload();
    Ok(landed)
}

/// Forget a source and delete its clone. The switches it left in
/// `hooks-state.json` are harmless — they name ids nothing produces — and are
/// kept, so re-adding the same repo restores what you had turned on.
pub fn remove(slug: &str) -> Result<(), String> {
    let mut file = read_sources_file(&sources_path());
    let before = file.sources.len();
    file.sources.retain(|s| s.slug() != slug);
    if file.sources.len() == before {
        return Err(format!(
            "no source {slug} — `bough hooks list` names the ones you have."
        ));
    }
    write_sources_file(&sources_path(), &file).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(repos_dir().join(slug));
    super::reload();
    Ok(())
}

/// The recorded sources, for `bough hooks list`.
pub fn sources() -> Vec<GitSource> {
    read_sources_file(&sources_path()).sources
}

/// The sources file as it stands, for tests and the panel.
pub fn sources_file() -> SourcesFile {
    read_sources_file(&sources_path())
}

/// Where a slug's clone lives.
pub fn clone_dir(slug: &str) -> PathBuf {
    repos_dir().join(slug)
}
