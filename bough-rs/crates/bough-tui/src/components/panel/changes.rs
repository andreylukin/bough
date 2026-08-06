//! The changes tab: what this session did to the checkout, and the one way to
//! undo it (port of `src/tui/components/Changes.tsx`).
//!
//! THE INVARIANT THIS HOLDS: **"not a repository" is an answer, never an empty
//! diff.** `available: false` carries a `reason` sentence written server-side,
//! and this file's first job is to show that sentence rather than fall through
//! to the file list. "This workspace is not a repository" and "you changed
//! nothing" are different facts.
//!
//! SECOND: **revert is the only mutation, and it is per path.** There is no
//! apply — the agent edits the user's checkout in place. And it takes TWO
//! keypresses: `x` arms a revert and prints what it will destroy, ⏎ performs
//! it, because a key that deletes a file on the first press is a key the
//! cursor lands on. The all-scope has its OWN key (`X`) and its own arm: the
//! escalation used to ride a second `x` — the same gesture the rail teaches as
//! "arm, then confirm" — which put the reflex one ⏎ from wiping the session's
//! work.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde::Deserialize;

use crate::ansi::wrap_line;
use crate::components::panel::{legend_line, paint_rows, window_around};
use crate::components::{ACCENT, ERROR, INFO, WARN};
use crate::store::selectors::{clip, plural};
use crate::store::state::SessionChangeSet;

// ---------------------------------------------------------------------------
// The wire shape (`vcs/repodiff.ts`), as this client reads it
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
}

impl FileStatus {
    fn word(self) -> &'static str {
        match self {
            FileStatus::Added => "added",
            FileStatus::Modified => "modified",
            FileStatus::Deleted => "deleted",
        }
    }

    fn mark(self) -> &'static str {
        match self {
            FileStatus::Added => "A",
            FileStatus::Modified => "M",
            FileStatus::Deleted => "D",
        }
    }

    fn color(self) -> ratatui::style::Color {
        match self {
            FileStatus::Deleted => ERROR,
            FileStatus::Added => ACCENT,
            FileStatus::Modified => WARN,
        }
    }
}

/// One `@@ … @@` block: the header verbatim plus the body lines with their
/// leading ` `/`+`/`-` markers intact.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<String>,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    /// Repo-relative, forward slashes — the same string `revert` takes back.
    pub path: String,
    pub status: FileStatus,
    pub hunks: Vec<Hunk>,
    /// The file is not text, so there are no hunks and none are coming — a
    /// separate fact from "no hunks".
    #[serde(default)]
    pub binary: Option<bool>,
}

// ---------------------------------------------------------------------------
// Pure core
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeItem {
    pub file: FileDiff,
    pub added: usize,
    pub removed: usize,
}

/// Added/removed line counts. `+++`/`---` never appear — the server sends
/// hunks only.
pub fn file_stats(f: &FileDiff) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for hunk in &f.hunks {
        for line in &hunk.lines {
            if line.starts_with('+') {
                added += 1;
            } else if line.starts_with('-') {
                removed += 1;
            }
        }
    }
    (added, removed)
}

/// The file list, in the server's order — git's, which is already path-sorted.
pub fn change_items(set: Option<&SessionChangeSet>) -> Vec<ChangeItem> {
    let Some(set) = set.filter(|s| s.available) else {
        return Vec::new();
    };
    set.files
        .iter()
        .filter_map(|v| serde_json::from_value::<FileDiff>(v.clone()).ok())
        .map(|file| {
            let (added, removed) = file_stats(&file);
            ChangeItem {
                file,
                added,
                removed,
            }
        })
        .collect()
}

/// One file's hunks flattened to display lines, headers included.
pub fn diff_body(f: Option<&FileDiff>) -> Vec<String> {
    let Some(f) = f else { return Vec::new() };
    if f.hunks.is_empty() {
        // BINARY IS ITS OWN ANSWER: an empty file and an unreadable one must
        // not give the reviewer the same sentence.
        if f.binary == Some(true) {
            return vec![format!(
                "(binary file — {}, contents not shown)",
                f.status.word()
            )];
        }
        return vec![format!("(no textual diff — {})", f.status.word())];
    }
    f.hunks
        .iter()
        .flat_map(|h| std::iter::once(h.header.clone()).chain(h.lines.iter().cloned()))
        .map(|l| printable(&l))
        .collect()
}

/// Control bytes out of a diff line, before it is painted.
///
/// The rows render `line` verbatim, so a file holding raw bytes — a stray
/// `\r`, a log with ANSI colour in it — put an ESC on the screen and the
/// terminal OBEYED it: a diff viewer moving the cursor and repainting the
/// frame. Tab is kept; it is layout.
fn printable(line: &str) -> String {
    line.chars()
        .map(|c| match c {
            '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}' => '·',
            other => other,
        })
        .collect()
}

/// A revert that has been asked for and not yet done. Two scopes and no third.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingRevert {
    File(ChangeItem),
    All,
}

/// What reverting THIS row will do, in words, before it is done.
///
/// The consent rule: a destructive verb names its own blast radius. "revert"
/// is not self-explanatory here — on a file this session ADDED it means
/// delete, and a dialog that does not say so is asking for a yes to a question
/// the user did not read.
pub fn revert_scope(item: &ChangeItem, total: usize) -> String {
    let what = match item.file.status {
        FileStatus::Added => "added by this session — reverting DELETES it".to_string(),
        FileStatus::Deleted => "deleted by this session — reverting RESTORES it".to_string(),
        FileStatus::Modified => format!(
            "modified by this session — reverting DISCARDS +{} -{}",
            item.added, item.removed
        ),
    };
    let rest = total.saturating_sub(1);
    if rest == 0 {
        return what;
    }
    format!(
        "{what}; the other {rest} file{} untouched",
        if rest == 1 { " is" } else { "s are" }
    )
}

/// Spec §13's non-git case: the agent works, it just produces nothing reviewable.
pub const NOT_A_REPO_HINT: &str =
    "the agent still works here — its edits just aren't reviewable, and revert is unavailable";

// ---------------------------------------------------------------------------
// Presentation
// ---------------------------------------------------------------------------

pub struct ChangesProps<'a> {
    /// `None` while the fetch is in flight — distinct from an unavailable set.
    pub set: Option<&'a SessionChangeSet>,
    pub items: &'a [ChangeItem],
    pub selected: usize,
    /// Lines scrolled into the hunk body of the selected file.
    pub scroll: usize,
    pub rows: usize,
    /// Hide the file list and give the whole tab to one file's hunks.
    pub focused: bool,
    /// Result of the last revert, or why one was refused.
    pub message: Option<&'a str>,
    /// The revert waiting for a yes.
    pub pending: Option<&'a PendingRevert>,
    /// The line printed under an unavailable change set. `None` suppresses it
    /// — the "no conversation is open" case has no checkout at all, and the
    /// non-git sentence would be a claim this component cannot make.
    pub hint: Option<&'a str>,
}

impl Default for ChangesProps<'_> {
    fn default() -> Self {
        ChangesProps {
            set: None,
            items: &[],
            selected: 0,
            scroll: 0,
            rows: 0,
            focused: false,
            message: None,
            pending: None,
            hint: Some(NOT_A_REPO_HINT),
        }
    }
}

/// The lines this tab paints, in order.
// `head_rows` keeps the empty and focused arms apart even though both are 1 —
// that is the ternary in src/tui/components/Changes.tsx:226, where the two cases
// are one row for different reasons.
#[allow(clippy::if_same_then_else)]
pub fn changes_lines(p: &ChangesProps, cols: usize) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let mut out: Vec<Line<'static>> = Vec::new();

    let Some(set) = p.set else {
        out.push(Line::from(Span::styled("loading changes…", dim)));
        return out;
    };
    if !set.available {
        let reason = set
            .reason
            .clone()
            .unwrap_or_else(|| "no change set here".to_string());
        for row in wrap_line(&reason, cols) {
            out.push(Line::from(Span::styled(row, Style::default().fg(WARN))));
        }
        if let Some(hint) = p.hint {
            for row in wrap_line(hint, cols) {
                out.push(Line::from(Span::styled(row, dim)));
            }
        }
        // Even with nothing to review the tab ends in a legend: "there is no
        // way out of here" is never true.
        out.push(Line::from(Span::styled("esc back · ^t close", dim)));
        return out;
    }

    // THE ROW BUDGET, COUNTED RATHER THAN GUESSED: message · the file list (a
    // header + up to six rows) · the diff (a blank separator + body + an
    // optional `— n/m —`) · the legend, or the 3-row confirm.
    let msg_rows = usize::from(p.message.is_some());
    // The dialog takes rows from the DIFF rather than from the bottom of the
    // screen: a confirm the panel scrolled off is a confirm nobody read.
    let foot_rows = if p.pending.is_some() { 3 } else { 1 };
    let list_rows = if p.focused {
        0
    } else {
        let avail = p.rows as isize - msg_rows as isize - foot_rows as isize - 2;
        p.items.len().min(6.min(avail).max(1) as usize)
    };
    let (start, _) = window_around(p.selected, p.items.len(), list_rows.max(1));
    let current = p.items.get(p.selected);
    let body = diff_body(current.map(|c| &c.file));
    let head_rows = if p.items.is_empty() {
        1
    } else if p.focused {
        1
    } else {
        1 + list_rows
    };
    // What is left for the diff, after its own blank separator row.
    let room = (p.rows as isize - msg_rows as isize - head_rows as isize - foot_rows as isize - 1)
        .max(0) as usize;
    let body_rows = if body.len() > room {
        room.saturating_sub(1)
    } else {
        room
    };
    let at = p.scroll.min(body.len().saturating_sub(body_rows));

    if let Some(message) = p.message {
        out.push(Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(WARN),
        )));
    }
    if p.items.is_empty() {
        out.push(Line::from(Span::styled(
            "no changes in this checkout yet",
            dim,
        )));
    } else if p.focused {
        if let Some(current) = current {
            out.push(Line::from(vec![
                Span::styled(
                    current.file.status.mark(),
                    Style::default().fg(current.file.status.color()),
                ),
                Span::raw(" "),
                Span::styled(current.file.path.clone(), bold),
            ]));
        }
    } else {
        let mut head = vec![
            Span::styled(p.items.len().to_string(), bold),
            Span::styled(
                format!(" file{} changed", if p.items.len() == 1 { "" } else { "s" }),
                dim,
            ),
        ];
        if let Some(base) = &set.base {
            head.push(Span::styled(
                format!("  since {}", base.chars().take(8).collect::<String>()),
                dim,
            ));
        }
        out.push(Line::from(head));
        for (i, item) in p.items.iter().skip(start).take(list_rows).enumerate() {
            let idx = start + i;
            let sel = idx == p.selected;
            let status_style = if sel {
                Style::default()
            } else {
                Style::default().fg(item.file.status.color())
            };
            out.push(Line::from(vec![
                Span::styled(
                    if sel { "❯ " } else { "  " },
                    if sel {
                        Style::default().fg(ACCENT)
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(item.file.status.mark(), status_style),
                Span::raw(" "),
                Span::styled(
                    clip(&item.file.path, 48),
                    if sel { bold } else { Style::default() },
                ),
                Span::styled(
                    format!("  +{}", item.added),
                    if sel {
                        Style::default()
                    } else {
                        Style::default().fg(ACCENT)
                    },
                ),
                Span::styled(
                    format!(" -{}", item.removed),
                    if sel {
                        Style::default()
                    } else {
                        Style::default().fg(ERROR)
                    },
                ),
            ]));
        }
    }

    if !body.is_empty() && body_rows > 0 {
        out.push(Line::default());
        for line in body.iter().skip(at).take(body_rows) {
            let style = if line.starts_with("@@") {
                Style::default().fg(INFO)
            } else if line.starts_with('+') {
                Style::default().fg(ACCENT)
            } else if line.starts_with('-') {
                Style::default().fg(ERROR)
            } else {
                dim
            };
            out.push(Line::from(Span::styled(
                if line.is_empty() {
                    " ".to_string()
                } else {
                    line.clone()
                },
                style,
            )));
        }
        if body.len() > body_rows {
            out.push(Line::from(Span::styled(
                format!("— {}/{} —", at + body_rows.min(body.len()), body.len()),
                dim,
            )));
        }
    }

    // The legend is the tab's LAST row, and it names the keys the keymap binds.
    match p.pending {
        Some(pending) => out.extend(revert_confirm(pending, p.items, set.base.as_deref())),
        None => {
            let items: Vec<String> = if p.focused {
                [
                    "← back",
                    "↑↓ scroll the diff",
                    "x revert this path",
                    "X revert everything",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect()
            } else {
                [
                    "↑↓ move",
                    "→ focus one file",
                    "x revert this path",
                    "X revert all",
                    "esc back",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect()
            };
            out.push(Line::from(Span::styled(
                legend_line(&items, Some(cols)),
                dim,
            )));
        }
    }
    out
}

/// The yes/no, printed where the legend was.
///
/// It replaces the legend rather than joining it, because the keys that mean
/// something while a revert is armed are not the keys that mean something
/// otherwise — a footer listing both would be listing the ones that are inert.
fn revert_confirm(
    pending: &PendingRevert,
    items: &[ChangeItem],
    base: Option<&str>,
) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    match pending {
        PendingRevert::All => {
            let added: usize = items.iter().map(|i| i.added).sum();
            let removed: usize = items.iter().map(|i| i.removed).sum();
            vec![
                Line::from(Span::styled(
                    // `plural`, because this sentence is the last thing read
                    // before work is destroyed: "revert all 1 files" reads as
                    // a placeholder.
                    format!(
                        "revert all {} (+{added} -{removed})?",
                        plural(items.len() as i64, "file")
                    ),
                    bold.fg(ERROR),
                )),
                Line::from(Span::styled(
                    format!(
                        "everything this session touched goes back{}, and files it created are deleted",
                        base.map(|b| format!(" to {}", b.chars().take(8).collect::<String>()))
                            .unwrap_or_default()
                    ),
                    dim,
                )),
                Line::from(vec![
                    Span::styled("⏎ revert everything", Style::default().fg(ERROR)),
                    Span::styled(" · esc cancel", dim),
                ]),
            ]
        }
        PendingRevert::File(item) => vec![
            Line::from(Span::styled(
                format!("revert {}?", item.file.path),
                bold.fg(WARN),
            )),
            Line::from(Span::styled(revert_scope(item, items.len()), dim)),
            Line::from(vec![
                Span::styled("⏎ revert it", Style::default().fg(WARN)),
                Span::styled(
                    format!(
                        "{}  ·  esc cancel",
                        // `X`, not a second `x`: the capital is a separate key
                        // and a separate decision.
                        if items.len() > 1 {
                            format!("  ·  X all {} files", items.len())
                        } else {
                            String::new()
                        }
                    ),
                    dim,
                ),
            ]),
        ],
    }
}

pub fn render_changes(p: &ChangesProps, cols: usize, area: Rect, buf: &mut Buffer) {
    paint_rows(&changes_lines(p, cols), area, buf);
}

// ---------------------------------------------------------------------------
// Tests — ported from src/tui/components/Panel.test.ts (changes cases)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::panel::test_render::draw_panel;
    use crate::components::panel::PanelBody;
    use crate::keys::PanelTab;
    use serde_json::json;

    fn set(available: bool, base: Option<&str>, files: Vec<serde_json::Value>) -> SessionChangeSet {
        SessionChangeSet {
            available,
            reason: if available {
                None
            } else {
                Some("this workspace is not a git repository".into())
            },
            base: base.map(|b| b.to_string()),
            files,
            workspace: Some("/tmp/x".into()),
        }
    }

    fn text_of(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn the_changes_tab_says_not_a_repository_rather_than_showing_an_empty_diff() {
        let unavailable = set(false, None, vec![]);
        assert!(change_items(Some(&unavailable)).is_empty());
        let body = PanelBody::Changes(ChangesProps {
            set: Some(&unavailable),
            rows: 12,
            ..Default::default()
        });
        let frame = draw_panel(PanelTab::Changes, &body, 100, 12).join("\n");
        assert!(frame.contains("not a git repository"), "{frame}");
        assert!(!frame.contains("files changed"), "{frame}");
        assert!(
            !frame.contains("no changes in this checkout yet"),
            "{frame}"
        );
        // The way out is always named.
        assert!(frame.contains("esc back · ^t close"), "{frame}");
    }

    #[test]
    fn the_no_checkout_case_suppresses_the_non_git_hint() {
        // "the agent still works here" is false when there is no workspace.
        let none = SessionChangeSet {
            available: false,
            reason: Some("no conversation is open — open one to review its changes".into()),
            base: None,
            files: vec![],
            workspace: None,
        };
        let lines = changes_lines(
            &ChangesProps {
                set: Some(&none),
                rows: 12,
                hint: None,
                ..Default::default()
            },
            80,
        );
        let text: Vec<String> = lines.iter().map(text_of).collect();
        assert!(
            text.iter().any(|l| l.contains("no conversation is open")),
            "{text:?}"
        );
        assert!(
            !text.iter().any(|l| l.contains("still works here")),
            "{text:?}"
        );
    }

    #[test]
    fn a_change_set_counts_its_own_plus_minus_and_flattens_hunks_for_display() {
        let file = json!({
            "path": "src/tui/theme.ts",
            "status": "modified",
            "hunks": [{"header": "@@ -1,3 +1,4 @@", "lines": [" keep", "-gone", "+new", "+also"]}],
        });
        let changes = set(true, Some("abcdef1234"), vec![file.clone()]);
        let parsed: FileDiff = serde_json::from_value(file).unwrap();
        assert_eq!(file_stats(&parsed), (2, 1));
        assert_eq!(
            diff_body(Some(&parsed)),
            vec!["@@ -1,3 +1,4 @@", " keep", "-gone", "+new", "+also"]
        );

        // A file with no hunks says so rather than rendering nothing…
        let empty: FileDiff =
            serde_json::from_value(json!({"path": "a.png", "status": "added", "hunks": []}))
                .unwrap();
        assert_eq!(diff_body(Some(&empty)), vec!["(no textual diff — added)"]);
        // …and a BINARY one says which.
        let binary: FileDiff = serde_json::from_value(
            json!({"path": "a.png", "status": "added", "hunks": [], "binary": true}),
        )
        .unwrap();
        assert_eq!(
            diff_body(Some(&binary)),
            vec!["(binary file — added, contents not shown)"]
        );

        // CONTROL BYTES NEVER REACH THE ROW. Tab survives; it is layout.
        let noisy: FileDiff = serde_json::from_value(json!({
            "path": "x.log",
            "status": "modified",
            "hunks": [{"header": "@@ -1 +1 @@", "lines": ["+red \u{1b}[31mtext\u{7}\r", "+keeps\ttab"]}],
        }))
        .unwrap();
        assert_eq!(
            diff_body(Some(&noisy)),
            vec!["@@ -1 +1 @@", "+red ·[31mtext··", "+keeps\ttab"]
        );

        let items = change_items(Some(&changes));
        let body = PanelBody::Changes(ChangesProps {
            set: Some(&changes),
            items: &items,
            rows: 14,
            ..Default::default()
        });
        let frame = draw_panel(PanelTab::Changes, &body, 100, 16).join("\n");
        assert!(frame.contains("theme.ts"), "{frame}");
        assert!(frame.contains("+2"), "{frame}");
        assert!(frame.contains("-1"), "{frame}");
        assert!(frame.contains("since abcdef12"), "{frame}");
    }

    fn item(path: &str, status: &str, added: usize, removed: usize) -> ChangeItem {
        ChangeItem {
            file: FileDiff {
                path: path.into(),
                status: serde_json::from_value(json!(status)).unwrap(),
                hunks: vec![],
                binary: None,
            },
            added,
            removed,
        }
    }

    #[test]
    fn a_revert_names_its_own_blast_radius_before_it_happens() {
        let added = item("new.ts", "added", 12, 0);
        assert_eq!(
            revert_scope(&added, 1),
            "added by this session — reverting DELETES it"
        );
        let deleted = item("gone.ts", "deleted", 0, 4);
        assert_eq!(
            revert_scope(&deleted, 2),
            "deleted by this session — reverting RESTORES it; the other 1 file is untouched"
        );
        let modified = item("a.ts", "modified", 3, 1);
        assert_eq!(
            revert_scope(&modified, 4),
            "modified by this session — reverting DISCARDS +3 -1; the other 3 files are untouched"
        );
    }

    #[test]
    fn the_two_press_idiom_prints_the_scope_and_x_does_not_widen_to_all() {
        let items = vec![item("a.ts", "modified", 3, 1), item("b.ts", "added", 9, 0)];
        let changes = set(true, Some("abcdef1234"), vec![]);
        let pending = PendingRevert::File(items[0].clone());
        let lines = changes_lines(
            &ChangesProps {
                set: Some(&changes),
                items: &items,
                rows: 14,
                pending: Some(&pending),
                ..Default::default()
            },
            80,
        );
        let text: Vec<String> = lines.iter().map(text_of).collect();
        assert!(text.iter().any(|l| l == "revert a.ts?"), "{text:?}");
        assert!(
            text.iter().any(|l| l.contains("DISCARDS +3 -1")),
            "{text:?}"
        );
        // ⏎ confirms THIS path; the all-scope is its own key, named here.
        let foot = text.last().unwrap();
        assert!(foot.starts_with("⏎ revert it"), "{foot}");
        assert!(foot.contains("X all 2 files"), "{foot}");
        assert!(foot.ends_with("esc cancel"), "{foot}");

        // The all-scope card counts the whole set and says what it deletes.
        let all = PendingRevert::All;
        let text: Vec<String> = changes_lines(
            &ChangesProps {
                set: Some(&changes),
                items: &items,
                rows: 14,
                pending: Some(&all),
                ..Default::default()
            },
            80,
        )
        .iter()
        .map(text_of)
        .collect();
        assert!(
            text.iter().any(|l| l == "revert all 2 files (+12 -1)?"),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|l| l.contains("goes back to abcdef12, and files it created are deleted")),
            "{text:?}"
        );
        assert!(
            text.last().unwrap().starts_with("⏎ revert everything"),
            "{text:?}"
        );
    }

    #[test]
    fn a_one_file_confirm_does_not_offer_an_all_scope_and_reads_as_one_file() {
        let items = vec![item("a.ts", "modified", 3, 1)];
        let changes = set(true, None, vec![]);
        let pending = PendingRevert::All;
        let text: Vec<String> = changes_lines(
            &ChangesProps {
                set: Some(&changes),
                items: &items,
                rows: 14,
                pending: Some(&pending),
                ..Default::default()
            },
            80,
        )
        .iter()
        .map(text_of)
        .collect();
        assert!(
            text.iter().any(|l| l == "revert all 1 file (+3 -1)?"),
            "{text:?}"
        );
        let file_pending = PendingRevert::File(items[0].clone());
        let text: Vec<String> = changes_lines(
            &ChangesProps {
                set: Some(&changes),
                items: &items,
                rows: 14,
                pending: Some(&file_pending),
                ..Default::default()
            },
            80,
        )
        .iter()
        .map(text_of)
        .collect();
        assert!(!text.last().unwrap().contains("X all"), "{text:?}");
    }

    #[test]
    fn the_confirm_takes_its_rows_from_the_diff_not_from_the_bottom_of_the_screen() {
        let hunk_lines: Vec<String> = (0..40).map(|i| format!(" line {i}")).collect();
        let file = json!({
            "path": "big.ts",
            "status": "modified",
            "hunks": [{"header": "@@ -1,40 +1,40 @@", "lines": hunk_lines}],
        });
        let changes = set(true, None, vec![file]);
        let items = change_items(Some(&changes));
        let plain = changes_lines(
            &ChangesProps {
                set: Some(&changes),
                items: &items,
                rows: 14,
                ..Default::default()
            },
            80,
        );
        let pending = PendingRevert::All;
        let armed = changes_lines(
            &ChangesProps {
                set: Some(&changes),
                items: &items,
                rows: 14,
                pending: Some(&pending),
                ..Default::default()
            },
            80,
        );
        // Both fit the budget; the confirm is two rows longer than the legend
        // and the DIFF is what shrank.
        assert!(plain.len() <= 14, "{}", plain.len());
        assert!(armed.len() <= 14, "{}", armed.len());
        let confirm = text_of(&armed[armed.len() - 3]);
        assert!(confirm.starts_with("revert all"), "{confirm}");
    }

    #[test]
    fn focus_mode_gives_the_tab_to_the_hunks_and_names_its_own_keys() {
        let file = json!({
            "path": "src/a.ts",
            "status": "modified",
            "hunks": [{"header": "@@ -1,2 +1,2 @@", "lines": ["-old", "+new"]}],
        });
        let changes = set(true, None, vec![file]);
        let items = change_items(Some(&changes));
        let text: Vec<String> = changes_lines(
            &ChangesProps {
                set: Some(&changes),
                items: &items,
                rows: 12,
                focused: true,
                ..Default::default()
            },
            80,
        )
        .iter()
        .map(text_of)
        .collect();
        assert_eq!(text[0], "M src/a.ts");
        assert!(text.iter().any(|l| l == "+new"), "{text:?}");
        let legend = text.last().unwrap();
        assert!(legend.starts_with("← back"), "{legend}");
        assert!(legend.contains("X revert everything"), "{legend}");
    }

    #[test]
    fn a_long_diff_says_how_far_down_it_is() {
        let hunk_lines: Vec<String> = (0..40).map(|i| format!(" line {i}")).collect();
        let file = json!({
            "path": "big.ts",
            "status": "modified",
            "hunks": [{"header": "@@ -1,40 +1,40 @@", "lines": hunk_lines}],
        });
        let changes = set(true, None, vec![file]);
        let items = change_items(Some(&changes));
        let text: Vec<String> = changes_lines(
            &ChangesProps {
                set: Some(&changes),
                items: &items,
                rows: 12,
                ..Default::default()
            },
            80,
        )
        .iter()
        .map(text_of)
        .collect();
        assert!(
            text.iter()
                .any(|l| l.starts_with("— ") && l.ends_with("/41 —")),
            "{text:?}"
        );
    }
}
