//! Invariant: one patch may carry several files and applies ALL of them or NONE. A conflict in
//! one file leaves every other file byte-identical.
//!
//! Order is load-bearing: parse, read every file, resolve every base, validate/rebase/assemble
//! every file — and only then write. A patch that fails on its third file has written nothing.

use std::collections::HashMap;
use std::path::PathBuf;

use bough_plugin_ledger::AgentName;
use bough_plugin_tools::WorkspaceRoot;

use super::grammar::{
    bad, check_ops, group_by_file, join_lines, materialize, normalize, parse_patch, tag_of,
    to_lines, OpKind, PatchError, PatchOp,
};
use super::rebase::{rebase_ops, RebaseResult};
use super::seen::SeenFiles;

/// What one file's application produced — echoed back so the next patch can chain onto the tag
/// without a re-view.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    pub path: String,
    /// The file's tag AFTER the patch.
    pub tag: String,
    pub lines_added: usize,
    pub lines_removed: usize,
    /// How many operations landed — the echo line reports it.
    pub ops: usize,
    /// The file's line count after the patch.
    pub lines: usize,
}

/// Parse, rebase, check and apply a whole patch, atomically across files.
///
/// The plan's signature was `(input, seen)`; a patch cannot be applied without the workspace root
/// to contain paths against and the agent whose views the tags anchor to, so both are arguments
/// (see the merge notes).
pub fn apply_patch(
    input: &str,
    root: &WorkspaceRoot,
    agent: &AgentName,
    seen: &SeenFiles,
) -> Result<Vec<Applied>, PatchError> {
    let ops = parse_patch(input)?;
    let groups = group_by_file(&ops)?;

    // Everything decided before anything is written.
    struct Decided {
        path: String,
        abs: PathBuf,
        text: String,
        ops: usize,
        added: usize,
        removed: usize,
    }
    let mut decided: Vec<Decided> = Vec::new();
    // absolute path → the section path that claimed it, so two spellings of one file are refused
    // rather than silently clobbering each other.
    let mut claimed: HashMap<PathBuf, String> = HashMap::new();

    for g in &groups {
        let abs = super::contain(root, &g.path).map_err(|detail| PatchError::Denied {
            path: g.path.clone(),
            detail,
        })?;
        if let Some(other) = claimed.get(&abs) {
            return bad(format!(
                "\"{other}\" and \"{}\" name the same file ({}) in one patch, so the second set \
                 of operations would be written against the version from before the first — \
                 silently discarding it. Nothing was written. Put all of that file's operations \
                 under a single \"[{other}#]\" section.",
                g.path,
                abs.display()
            ));
        }
        claimed.insert(abs.clone(), g.path.clone());

        let bytes = std::fs::read(&abs).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                PatchError::Io(format!(
                    "cannot patch {}: no such file (looked at {}). patch edits a file that \
                     exists — create it with write(\"{}\", …) instead. Nothing was written; a \
                     patch applies to all its files or none.",
                    g.path,
                    abs.display(),
                    g.path
                ))
            } else {
                PatchError::Io(format!(
                    "cannot patch {}: {e}. Nothing was written; a patch applies to all its files \
                     or none.",
                    g.path
                ))
            }
        })?;
        let current = String::from_utf8_lossy(&bytes).into_owned();

        // Absent = never viewed. There is no base to rebase from, and applying against the
        // current text is exactly the silent clobber this module exists to prevent.
        let (_, base_text) = seen.recall(agent, &abs).ok_or_else(|| PatchError::Unseen {
            path: g.path.clone(),
        })?;
        if !g.tag.is_empty() && tag_of(&base_text) != g.tag {
            return Err(PatchError::StaleTag {
                path: g.path.clone(),
                saw: g.tag.clone(),
                now: tag_of(&current),
            });
        }

        let base_lines = to_lines(&base_text);
        // Bounds and overlap are judged in the coordinates the ops were WRITTEN in — the viewed
        // version, not whatever the file has since become.
        check_ops(&g.path, &g.ops, base_lines.len())?;

        let current_lines = to_lines(&current);
        let effective: Vec<PatchOp> = if normalize(&base_text) != normalize(&current) {
            match rebase_ops(&g.ops, &base_lines, &current_lines) {
                RebaseResult::Conflict(c) => return Err(c.into()),
                RebaseResult::Rebased(ops) => ops,
                RebaseResult::Unchanged => g.ops.clone(),
            }
        } else {
            g.ops.clone()
        };

        let (added, removed) = counts(&effective);
        decided.push(Decided {
            path: g.path.clone(),
            abs,
            text: join_lines(&materialize(&current_lines, &effective), &current),
            ops: g.ops.len(),
            added,
            removed,
        });
    }

    let mut written: Vec<String> = Vec::new();
    let mut out: Vec<Applied> = Vec::new();
    for d in &decided {
        if let Err(e) = std::fs::write(&d.abs, &d.text) {
            // Every file was decided before any was written, so this is a filesystem failure, not
            // a patch decision. Say exactly how far it got.
            let landed = if written.is_empty() {
                "Nothing was written.".to_string()
            } else {
                format!(
                    "Already written and NOT rolled back: {} — re-view those before editing them \
                     again.",
                    written.join(", ")
                )
            };
            return Err(PatchError::Io(format!(
                "cannot write {}: {e}. {landed} The remaining files in this patch were not \
                 written.",
                d.path
            )));
        }
        // What this agent last saw at the path is now what it just wrote, so the echoed tag is
        // live: a follow-up patch may anchor to it without viewing again.
        let tag = tag_of(&d.text);
        seen.remember(agent.clone(), d.abs.clone(), tag.clone(), d.text.clone());
        written.push(d.path.clone());
        out.push(Applied {
            path: d.path.clone(),
            tag,
            lines_added: d.added,
            lines_removed: d.removed,
            ops: d.ops,
            lines: to_lines(&d.text).len(),
        });
    }
    Ok(out)
}

/// Lines this patch adds and removes, counted from the operations themselves.
fn counts(ops: &[PatchOp]) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for op in ops {
        match op.kind {
            OpKind::Del => {
                removed += op.b.unwrap_or(0) - op.a.unwrap_or(0) + 1;
            }
            OpKind::Swap => {
                removed += op.b.unwrap_or(0) - op.a.unwrap_or(0) + 1;
                added += op.body.len();
            }
            _ => added += op.body.len(),
        }
    }
    (added, removed)
}

/// The model-facing echo for one applied file.
pub fn echo(a: &Applied) -> String {
    format!(
        "[{}#{}] patched — {}, now {}",
        a.path,
        a.tag,
        plural(a.ops, "operation"),
        plural(a.lines, "line")
    )
}

pub(crate) fn plural(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}
