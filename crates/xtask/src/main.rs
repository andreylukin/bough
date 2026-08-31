//! `cargo xtask events [--check] [--write <path>]` — §15 item 7's event catalog gate.
//!
//! `--check` prints the findings and exits non-zero if there are any; `--write` regenerates the
//! committed catalog. With neither, the table goes to stdout.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    match it.next().map(String::as_str) {
        Some("events") => {}
        other => bail!(
            "usage: cargo xtask events [--check] [--write <path>] (got {:?})",
            other.unwrap_or("nothing")
        ),
    }
    let mut do_check = false;
    let mut write: Option<PathBuf> = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--check" => do_check = true,
            "--write" => {
                write = Some(PathBuf::from(match it.next() {
                    Some(p) => p.clone(),
                    None => xtask::CATALOG_PATH.to_string(),
                }))
            }
            other => bail!("unknown argument {other:?}"),
        }
    }

    let root = workspace_root()?;
    let roots: Vec<PathBuf> = xtask::ROOTS.iter().map(|r| root.join(r)).collect();
    let refs: Vec<&Path> = roots.iter().map(|p| p.as_path()).collect();
    let catalog = xtask::scan(&refs)?;
    let findings = xtask::check(&catalog);
    let rendered = xtask::table(&catalog);

    if let Some(path) = &write {
        let path = if path.is_absolute() {
            path.clone()
        } else {
            root.join(path)
        };
        std::fs::write(&path, &rendered).with_context(|| format!("writing {}", path.display()))?;
        eprintln!("wrote {}", path.display());
    } else if !do_check {
        print!("{rendered}");
    }

    if do_check {
        for f in &findings {
            eprintln!("event-catalog: {f}");
        }
        if !findings.is_empty() {
            bail!("{} event catalog finding(s)", findings.len());
        }
        eprintln!(
            "event-catalog: {} events, {} sites, no findings",
            xtask::event_count(&catalog),
            catalog.sites.len()
        );
    }
    Ok(())
}

/// The workspace root: `CARGO_MANIFEST_DIR` is `crates/xtask`.
fn workspace_root() -> anyhow::Result<PathBuf> {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = here
        .parent()
        .and_then(Path::parent)
        .context("crates/xtask has no workspace root above it")?;
    Ok(root.to_path_buf())
}
