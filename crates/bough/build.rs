//! Stamps the build's git identity into the binary, so `bough --version`
//! answers with something a bug report can be pinned to.
//!
//! WHY THIS EXISTS. bough publishes no binaries and installs by building
//! `main`, so "0.1.0" identifies nothing — every install since the first commit
//! says it. The crate version tells you which release line; the sha tells you
//! which build, and that is the one a reporter and a maintainer have to agree
//! on. `-dirty` is part of it: a local build with uncommitted changes is not
//! the commit it claims, and that distinction is exactly where a "cannot
//! reproduce" comes from.
//!
//! Absence is never fatal. A source tarball, a vendored build, or a machine
//! with no `git` yields no sha and `--version` prints the bare crate version —
//! the same answer it gave before this file existed.

use std::process::Command;

fn main() {
    // Re-run when HEAD moves. `.git/HEAD` covers a checkout or a commit;
    // `.git/index` covers staging, which is what flips `-dirty`. Neither is
    // present in a tarball, and `cargo:rerun-if-changed` on a missing path is
    // not an error — it just means this script runs once.
    let git_dir = std::path::Path::new("../../.git");
    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    println!("cargo:rerun-if-changed={}", git_dir.join("index").display());

    if let Some(describe) = git_describe() {
        println!("cargo:rustc-env=BOUGH_BUILD_REV={describe}");
    }
}

/// `<short-sha>`, plus `-dirty` when the working tree has uncommitted changes.
///
/// Deliberately NOT `git describe --tags`: the tag is what the *installer*
/// pins (see `install.sh`), and a describe string like `v0.1.0-14-gabc1234`
/// puts two version numbers in one line that can disagree. The crate version
/// says which line, the sha says which build. One question each.
fn git_describe() -> Option<String> {
    let sha = git(&["rev-parse", "--short=9", "HEAD"])?;
    // `--quiet` exits 1 when there is a diff and prints nothing, so the exit
    // status IS the answer. A failure to run at all (no git) is treated as
    // clean rather than guessed at as dirty.
    let dirty = match Command::new("git")
        .args(["diff", "--quiet", "HEAD", "--"])
        .status()
    {
        Ok(status) => !status.success(),
        Err(_) => false,
    };
    Some(match dirty {
        true => format!("{sha}-dirty"),
        false => sha,
    })
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}
