//! The optional llmwiki bridge.
//!
//! WHY A BRIDGE AND NOT A REIMPLEMENTATION. `llmwiki-cli` already maintains
//! `index.md`, resolves `[[wikilinks]]`, and answers `backlinks` / `orphans` /
//! `lint` / `search` over a directory of markdown with YAML frontmatter —
//! which is exactly what [`super::save`] writes. Registering `~/.bough/notes`
//! as a second wiki gets all of that for the cost of one shell-out.
//!
//! OWNERSHIP IS SPLIT, and the split is not arbitrary:
//!
//!   * **page creation** goes through `wiki write`, because that is what
//!     upserts `index.md`;
//!   * **appends** are a direct file write, because llmwiki has no append verb
//!     (its only write mode is whole-page replace) and an append changes
//!     nothing the index records.
//!
//! EVERY PATH DEGRADES. llmwiki is not a dependency of the note memory — it is
//! an enhancement of it. With the CLI absent, notes are still written, read,
//! appended, listed and searched by bough itself; only `index.md` goes
//! unmaintained, which `wiki lint` fixes whenever the CLI reappears. Nothing
//! here ever fails a turn.

use std::path::Path;
use std::process::Command;

/// The registry id bough claims. Distinct from whatever the user's own wiki is
/// called, so `wiki -w bough` never touches a personal knowledge base.
pub const WIKI_ID: &str = "bough";

/// Is the CLI on PATH? Cheap enough to ask per call and not worth caching —
/// the answer changes when the user installs it, and a stale "no" would make
/// the feature stay broken after the fix.
pub fn available() -> bool {
    which().is_some()
}

fn which() -> Option<&'static str> {
    for bin in ["wiki", "llmwiki"] {
        let ok = Command::new(bin)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            // Leaked so callers get a `&'static str`; two possible values, once.
            return Some(if bin == "wiki" { "wiki" } else { "llmwiki" });
        }
    }
    None
}

/// The one-line install hint, used wherever a verb needed the CLI and did not
/// find it. Names the command AND what is lost, so the reader can decide
/// whether they care.
pub const INSTALL_HINT: &str = "llmwiki-cli is not installed, so index.md, backlinks and lint are \
                                unavailable — notes themselves work without it. \
                                Install: npm install -g llmwiki-cli";

/// Create the wiki and register it, if it is not there already. Idempotent and
/// best-effort: a failure leaves a perfectly usable directory of markdown.
pub fn ensure_wiki(root: &Path) -> Result<(), String> {
    if root.join(".llmwiki.yaml").exists() {
        return Ok(());
    }
    let Some(bin) = which() else {
        return Err(INSTALL_HINT.to_string());
    };
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let out = Command::new(bin)
        .arg("init")
        .arg(root)
        .args(["--name", WIKI_ID])
        .args(["--domain", "engineering memory"])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    // `wiki init` writes a SCHEMA.md for a PERSONAL knowledge base:
    // entities/concepts/sources/synthesis, and a set of example wikilinks that
    // point at nothing. Left in place it is seven of the eight issues the very
    // first `bough notes lint` reports, which trains the reader to ignore the
    // command. Replaced with the conventions that actually apply here.
    let _ = std::fs::write(root.join("SCHEMA.md"), SCHEMA);
    Ok(())
}

/// What `bough notes lint` should be linting against. Carries frontmatter and
/// no wikilinks, so it is neither an orphan nor a source of broken links.
const SCHEMA: &str = r#"---
title: bough note conventions
tags: [meta]
---

# bough — note memory

Written and read by `bough notes`. The pages here are prose keyed on the tags
in bough's command memory; `bough tags show TAG` prints the matching note above
the commands.

## Layout

    wiki/refs/<reference>.md   a ticket, a PR — anything with an id outside bough
    wiki/tags/<tag>.md         a subsystem or a word this project uses

## The rule that matters

A note holds WHY. It never holds a command, a command's output, or an exit
code — those are in `command_history`, and `bough tags show <tag>` is the
citation. Two records of one fact age apart; one record and a pointer does not.

## The two zones

Everything above `## Log` is prose a human or the session model wrote, replaced
whole by `bough notes write`. The `## Log` section is derived from the command
memory, appended one line at a time, and thrown away by
`bough notes rebuild <tag>`.

Line prefixes say who wrote a log line: `*` you, `+` the session model,
`~` the cheap model.

A `> [!WARNING]` callout in the prose means the log contradicts the claim above
it. Only a human or the session model may resolve one.

## Frontmatter

`title`, `key` (the tag), and `synced` — a per-machine map of the last command
timestamp folded into the log. It is a map because this directory syncs between
machines and the command memory does not.
"#;

/// Tell llmwiki a page exists, so `index.md` gains its line.
///
/// Called AFTER [`super::save`] has written the file, and handed the same
/// title — llmwiki rewrites the page from this JSON, so the content passed
/// here must be what is already on disk. Best-effort by contract.
pub fn index_page(root: &Path, rel_path: &str, title: &str, content: &str) -> Result<(), String> {
    let Some(bin) = which() else {
        return Err(INSTALL_HINT.to_string());
    };
    let body = serde_json::json!({ "title": title, "content": content }).to_string();
    let mut child = Command::new(bin)
        .args(["-w", WIKI_ID, "write", rel_path])
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().ok_or("no stdin")?;
        stdin
            .write_all(body.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// `wiki lint` over the bough wiki. Returns its report, or the reason there
/// isn't one.
pub fn lint(root: &Path) -> Result<String, String> {
    let Some(bin) = which() else {
        return Err(INSTALL_HINT.to_string());
    };
    let out = Command::new(bin)
        .args(["-w", WIKI_ID, "lint"])
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        Ok(text)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// The wiki-relative path of a page, as llmwiki wants it.
pub fn rel_path(key: &str) -> String {
    format!("wiki/{}/{}.md", super::dir_for(key), key.replace('/', "-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_relative_path_matches_where_save_puts_the_file() {
        let root = Path::new("/n");
        for key in ["linear.nme-1673", "nased", "branch.claude/tags-history"] {
            let abs = super::super::path_for(root, key);
            assert!(
                abs.ends_with(rel_path(key)),
                "{key}: {} vs {}",
                abs.display(),
                rel_path(key)
            );
        }
    }

    #[test]
    fn the_install_hint_says_what_still_works() {
        // The message is a product surface: a reader who does not want the
        // graph must be able to tell that they lose nothing they use.
        assert!(INSTALL_HINT.contains("notes themselves work without it"));
        assert!(INSTALL_HINT.contains("npm install -g llmwiki-cli"));
    }
}
