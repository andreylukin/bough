//! Invariant: switching the pane to a branch is a PANE-LOCAL trajectory override, never a
//! `FocusRequest`. A fork has no `agents` row (that is what makes it a fork rather than a lane),
//! so there is nothing for the shell to focus; and the agent's OWN chain is remembered so `Esc`
//! always returns to it, whatever the picker did.
//!
//! Everything here is pure: edges + labels in, a list and a cursor out.

use bough_plugin_ledger::{AgentName, Edge, EdgeKind, Seq, TrajId};
use bough_plugin_tui_shell::Theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// One branch of the focused agent's trajectory.
#[derive(Clone, Debug, PartialEq)]
pub struct Branch {
    pub traj: TrajId,
    /// Where it left the parent.
    pub at_seq: Seq,
    /// How many steps it holds, for "is there anything on it".
    pub steps: usize,
    /// `Some` iff an `agents` row names this trajectory: a LANE. `None` is a fork — a branch
    /// nobody lives on, promotable by adding the row (§4).
    pub lane: Option<AgentName>,
}

impl Branch {
    /// The word that separates the two kinds on screen.
    pub fn word(&self) -> &'static str {
        match self.lane {
            Some(_) => "lane",
            None => "fork",
        }
    }
}

/// PURE: the ancestor CHILDREN of one trajectory, oldest first.
///
/// A merge is excluded, and excluding it takes BOTH clauses: `graph-ops` writes a merge head an
/// `EdgeKind::Merge` edge AND an `EdgeKind::Ancestor` edge to each parent (the ancestor edge is
/// what keeps the merged past readable, since `connected()` follows ancestry only), so filtering
/// on the kind alone would show a merge head as a birth. A child that has a merge edge to this
/// parent is a merge, whatever else it also has.
///
/// A merge edge points at history that flowed IN, and offering it as a branch to switch to would
/// show the same steps under two names. Oldest first is by `at_seq` — where the branch left the
/// parent — with the trajectory id breaking ties, so the order is total and a redraw never
/// reshuffles the list under the cursor.
pub fn branches_from_edges(
    edges: &[Edge],
    parent: &TrajId,
    lane_of: &dyn Fn(&TrajId) -> Option<AgentName>,
    steps_of: &dyn Fn(&TrajId) -> usize,
) -> Vec<Branch> {
    let merged: std::collections::BTreeSet<&TrajId> = edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Merge && e.parent == *parent)
        .map(|e| &e.child)
        .collect();
    let mut out: Vec<Branch> = edges
        .iter()
        .filter(|e| {
            e.kind == EdgeKind::Ancestor && e.parent == *parent && !merged.contains(&e.child)
        })
        .map(|e| Branch {
            traj: e.child.clone(),
            at_seq: e.at_seq,
            steps: steps_of(&e.child),
            lane: lane_of(&e.child),
        })
        .collect();
    out.sort_by(|a, b| a.at_seq.cmp(&b.at_seq).then_with(|| a.traj.cmp(&b.traj)));
    out
}

/// The picker's whole state. Closed and empty is the resting position.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BranchPicker {
    pub open: bool,
    pub branches: Vec<Branch>,
    pub cursor: usize,
}

/// What a key in the picker asks the pane to do.
#[derive(Clone, Debug, PartialEq)]
pub enum PickerOutcome {
    /// The key was not the picker's.
    Ignored,
    /// Redraw; nothing else changed.
    Moved,
    /// Show this trajectory in the pane (a pane-local override).
    Show(TrajId),
    /// Close, and go back to the agent's own chain.
    Restore,
}

impl BranchPicker {
    /// Open over a freshly computed list. The cursor starts at the top.
    pub fn open_with(&mut self, branches: Vec<Branch>) {
        self.branches = branches;
        self.cursor = 0;
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn selected(&self) -> Option<&Branch> {
        self.branches.get(self.cursor)
    }

    /// PURE: a key ⇒ what the pane does. `Esc` on an OPEN picker closes it AND returns to the
    /// agent's own chain, which is the one gesture that always gets Andrey back (§11).
    pub fn on_key(&mut self, key: KeyEvent) -> PickerOutcome {
        if !self.open {
            return PickerOutcome::Ignored;
        }
        match key.code {
            KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                PickerOutcome::Moved
            }
            KeyCode::Down => {
                if self.cursor + 1 < self.branches.len() {
                    self.cursor += 1;
                }
                PickerOutcome::Moved
            }
            KeyCode::Enter => match self.selected().map(|b| b.traj.clone()) {
                Some(traj) => {
                    self.open = false;
                    PickerOutcome::Show(traj)
                }
                // An empty picker: Enter has nothing to select, so it behaves as Esc rather than
                // leaving Andrey stuck in a list of nothing.
                None => {
                    self.open = false;
                    PickerOutcome::Restore
                }
            },
            KeyCode::Esc => {
                self.open = false;
                PickerOutcome::Restore
            }
            _ => PickerOutcome::Ignored,
        }
    }

    /// PURE: the picker as lines. An agent with no children renders the empty picker rather than
    /// nothing at all — "no branches" is an answer.
    pub fn lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        let mut out = vec![Line::styled(
            "branches (↑/↓, Enter to show, Esc to return)",
            Style::default().fg(theme.dim),
        )];
        if self.branches.is_empty() {
            out.push(Line::styled(
                "  no branches",
                Style::default().fg(theme.dim),
            ));
            return out;
        }
        for (i, b) in self.branches.iter().enumerate() {
            let sel = i == self.cursor;
            let mut style = Style::default().fg(theme.fg);
            if sel {
                style = style.add_modifier(Modifier::BOLD).bg(theme.sel_bg);
            }
            let label = format!(
                "{} {}  {}  at seq {}  {} steps",
                if sel { '▸' } else { ' ' },
                b.word(),
                b.traj,
                b.at_seq.0,
                b.steps
            );
            let label: String = label.chars().take(width.max(1) as usize).collect();
            out.push(Line::from(Span::styled(label, style)));
        }
        out
    }
}
