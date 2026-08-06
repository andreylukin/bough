//! The panel's controller: the cursor, the fetch-on-entry triggers and the
//! confirm dispatch (port of the wave-2 half of
//! `src/tui/components/PanelHost.tsx`).
//!
//! ONE CURSOR, reset to 0 on every tab change and every open — EXCEPT the
//! tree, which lands on the row where `current` is true, because the switcher
//! must land on you-are-here. Arrival also clears: the message, the diff
//! focus, the diff scroll and the pending revert.
//!
//! WHAT IS NOT HERE YET is stated out loud rather than faked: the surgery
//! verbs (`e` split, `m` bring-here, `s` summarize-fork) and the fork ⏎ puts a
//! sentence in the tab's message row — "not wired into this client yet" is the
//! codebase's own idiom for an absent capability, and it is the one answer
//! that cannot mislead.

use std::collections::{HashMap, HashSet};

use bough_core::schema::parts::Message;

use crate::api::SessionRow;
use crate::forest::{
    forest_rows, reveal_path, selection_for, ForestInput, ForestRow, Selection,
};
use crate::keys::{Command, PanelTab};
use crate::store::state::SessionChangeSet;

use super::changes::{change_items, ChangeItem, PendingRevert};
use super::tree::forest_window;
use super::{panel_action_for, PanelAction, PanelState, INITIAL_PANEL};

/// What the panel needs the app to do. Every one is a REST call the loop owns;
/// the host itself performs no I/O.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostRequest {
    /// `GET /sessions` — the tree's rows, on entry.
    LoadSessions,
    /// `GET /sessions/:id/changes` — the change set, on entry.
    LoadChanges,
    /// Open this conversation and close the panel.
    Open(String),
    /// `POST /sessions/:id/changes/revert`. `None` = the whole set; the server
    /// refuses `[]`, so an empty list is never sent.
    Revert(Option<Vec<String>>),
    /// `GET /theme` — the palette in force, on entry to the theme tab.
    LoadTheme,
    /// Persist the kept palette. The verb is decided by
    /// `theme::persist_request`, never here.
    SaveTheme(crate::theme::ThemeWrite),
}

/// The concrete preview, seen through the panel's own trait. The widths differ
/// (`isize` at the seam, `i64` in theme.rs) so the bridge is explicit rather
/// than a type alias that would silently truncate on a 32-bit host.
impl super::ThemePreview for crate::theme::ThemePreview {
    fn move_by(&mut self, delta: isize) {
        crate::theme::ThemePreview::move_by(self, delta as i64);
    }
    fn commit(&mut self) {
        crate::theme::ThemePreview::commit(self);
    }
    fn cancel(&mut self) {
        crate::theme::ThemePreview::cancel(self);
    }
}

pub struct PanelHost {
    pub state: PanelState,
    /// The one cursor.
    pub sel: usize,
    /// A refusal or a result, shown in the tab's own message row.
    pub message: Option<String>,
    // ---- the tree ----------------------------------------------------------
    pub sessions: Vec<SessionRow>,
    pub threads: HashMap<String, Vec<Message>>,
    pub expanded: HashSet<String>,
    pub drilled: HashSet<String>,
    pub current_id: Option<String>,
    pub workspace: Option<String>,
    // ---- the changes tab ---------------------------------------------------
    pub changes: Option<SessionChangeSet>,
    pub items: Vec<ChangeItem>,
    /// The diff has the tab, and the wheel.
    pub diff_focused: bool,
    pub diff_scroll: usize,
    /// The revert waiting for a yes. `x` arms it; ⏎ performs it, esc cancels.
    pub pending: Option<PendingRevert>,
    // ---- the theme tab -----------------------------------------------------
    /// The browsing session, alive only while the palette in force is known.
    /// It is NOT rebuilt on every arrival: a preview rebuilt per entry would
    /// forget the baseline it owes a revert to.
    pub theme: Option<crate::theme::ThemePreview>,
}

impl Default for PanelHost {
    fn default() -> Self {
        PanelHost {
            state: INITIAL_PANEL,
            sel: 0,
            message: None,
            sessions: Vec::new(),
            threads: HashMap::new(),
            expanded: HashSet::new(),
            drilled: HashSet::new(),
            current_id: None,
            workspace: None,
            changes: None,
            items: Vec::new(),
            diff_focused: false,
            diff_scroll: 0,
            pending: None,
            theme: None,
        }
    }
}

impl PanelHost {
    pub fn open(&self) -> bool {
        self.state.open
    }

    pub fn tab(&self) -> PanelTab {
        self.state.tab
    }

    /// The forest, rebuilt from what has been fetched. Cheap enough to do per
    /// frame, and it must be the SAME list the digit resolver walks.
    pub fn rows(&self) -> Vec<ForestRow> {
        let no_children: HashMap<String, Vec<SessionRow>> = HashMap::new();
        forest_rows(&ForestInput {
            sessions: &self.sessions,
            children_by_origin: &no_children,
            threads: &self.threads,
            expanded: &self.expanded,
            drilled: &self.drilled,
            current_id: self.current_id.as_deref(),
            ..Default::default()
        })
    }

    /// The change set the fetch answered with, folded into rows.
    pub fn set_changes(&mut self, set: Option<SessionChangeSet>) {
        self.items = change_items(set.as_ref());
        self.changes = set;
        self.sel = self.sel.min(self.items.len().saturating_sub(1));
        self.pending = None;
    }

    /// The sessions the fetch answered with. Seeds the expansion so the open
    /// conversation is on screen — a handoff, a fork and a compaction all hang
    /// under what they came from, so without this the tree shows everything
    /// except where you are.
    pub fn set_sessions(&mut self, sessions: Vec<SessionRow>) {
        let no_children: HashMap<String, Vec<SessionRow>> = HashMap::new();
        for id in reveal_path(&sessions, &no_children, self.current_id.as_deref()) {
            self.expanded.insert(id);
        }
        self.sessions = sessions;
        if self.state.open && self.state.tab == PanelTab::Tree {
            self.land_on_current();
        }
    }

    /// The switcher lands on you-are-here.
    fn land_on_current(&mut self) {
        let rows = self.rows();
        self.sel = rows
            .iter()
            .position(|r| matches!(r, ForestRow::Session { current: true, .. }))
            .unwrap_or(0);
    }

    /// Arrival: one cursor, reset — and everything a previous tab armed,
    /// cleared. A confirmation that outlives the row it was read on is not a
    /// confirmation.
    fn arrive(&mut self, tab: PanelTab) -> Vec<HostRequest> {
        self.sel = 0;
        self.message = None;
        self.diff_focused = false;
        self.diff_scroll = 0;
        self.pending = None;
        match tab {
            PanelTab::Tree => {
                self.land_on_current();
                vec![HostRequest::LoadSessions]
            }
            PanelTab::Changes => vec![HostRequest::LoadChanges],
            // The cursor is the PREVIEW's, not the panel's: it starts on the
            // theme in force, so `sel` is left at 0 and never read here.
            PanelTab::Theme => vec![HostRequest::LoadTheme],
            _ => Vec::new(),
        }
    }

    /// Seed the browsing session from `GET /theme`. Re-entering the tab must
    /// not re-seed — that would move the baseline onto a palette the user is
    /// only previewing — so an existing preview is kept.
    pub fn set_theme(&mut self, state: Option<crate::theme::ThemeState>) {
        if self.theme.is_none() {
            self.theme = Some(crate::theme::ThemePreview::new(state));
        }
    }

    /// Unwind exactly ONE level, nearest state first. Returns whether it ate
    /// the keypress.
    fn back(&mut self) -> bool {
        if self.pending.is_some() {
            self.pending = None;
            return true;
        }
        if self.diff_focused {
            self.diff_focused = false;
            return true;
        }
        false
    }

    fn row_count(&self) -> usize {
        match self.state.tab {
            PanelTab::Tree => self.rows().len(),
            PanelTab::Changes => self.items.len(),
            _ => 0,
        }
    }

    /// `reduce_panel` with the preview attached. The preview travels WITH
    /// every state change because that function is the ONE place that knows
    /// you left the theme tab, and there are five ways to leave it.
    fn reduce_with_theme(&mut self, action: PanelAction) -> PanelState {
        let theme = self.theme.as_mut().map(|t| t as &mut dyn super::ThemePreview);
        super::reduce_panel(self.state, action, theme)
    }

    fn move_to(&mut self, at: isize) {
        let last = self.row_count().saturating_sub(1) as isize;
        self.sel = at.clamp(0, last.max(0)) as usize;
        // The cursor moving is also the arming being dropped.
        self.pending = None;
    }

    /// One resolved command. Returns the REST calls the app must make; an
    /// empty result may still have changed what is on screen.
    ///
    /// `digit` is the 1-9 the keypress carried (the keymap resolves every
    /// digit to one command, so the value has to travel beside it), and
    /// `body_rows` is the SAME budget the renderer paints with — two
    /// derivations of "which rows are visible" is how a digit comes to select
    /// a row nobody can see.
    pub fn handle(
        &mut self,
        command: Command,
        digit: Option<usize>,
        body_rows: usize,
    ) -> Vec<HostRequest> {
        // 1. A close while something is drilled or armed unwinds one level.
        if command == Command::PanelClose && self.back() {
            return Vec::new();
        }
        if let Some(action) = panel_action_for(command) {
            let before = self.state;
            match action {
                PanelAction::Move(delta) => {
                    if self.state.tab == PanelTab::Changes && self.diff_focused {
                        self.diff_scroll =
                            (self.diff_scroll as isize + delta).max(0) as usize;
                    } else if self.state.tab == PanelTab::Theme {
                        // Moving here PAINTS, and `reduce_panel` is where that
                        // is written — the preview owns its own cursor
                        // (clamped, never wrapping), so the panel's `sel` is
                        // deliberately not advanced beside it.
                        self.state = self.reduce_with_theme(PanelAction::Move(delta));
                    } else {
                        self.move_to(self.sel as isize + delta);
                    }
                    return Vec::new();
                }
                PanelAction::Confirm => return self.confirm(self.sel),
                PanelAction::ConfirmSummarize => {
                    // Elsewhere it must NOT run the ordinary commit — `s` in
                    // the model picker used to silently pin a model.
                    if self.state.tab == PanelTab::Tree {
                        self.message = Some(NOT_WIRED_SUMMARIZE.to_string());
                    }
                    return Vec::new();
                }
                other => {
                    self.state = self.reduce_with_theme(other);
                }
            }
            // Arrival is a tab change OR an open — never a close.
            if self.state.open && (!before.open || before.tab != self.state.tab) {
                return self.arrive(self.state.tab);
            }
            return Vec::new();
        }

        match command {
            // 6. Panel-open-only movement.
            Command::MoveIn => {
                if self.state.tab == PanelTab::Changes {
                    if !self.items.is_empty() {
                        self.diff_focused = true;
                    }
                    return Vec::new();
                }
                if self.state.tab == PanelTab::Tree {
                    match self.rows().get(self.sel) {
                        Some(ForestRow::Session { id, .. }) => {
                            self.expanded.insert(id.clone());
                        }
                        Some(ForestRow::Collapsed { origin_id, .. }) => {
                            self.drilled.insert(origin_id.clone());
                        }
                        _ => {}
                    }
                }
                Vec::new()
            }
            Command::MoveOut => {
                if self.back() {
                    return Vec::new();
                }
                if self.state.tab == PanelTab::Tree {
                    match self.rows().get(self.sel) {
                        Some(ForestRow::Session { id, .. }) => {
                            let id = id.clone();
                            self.expanded.remove(&id);
                            self.drilled.remove(&id);
                        }
                        // A turn or a caption closes ITS conversation.
                        Some(ForestRow::Message { session_id, .. })
                        | Some(ForestRow::Section { session_id, .. }) => {
                            let id = session_id.clone();
                            self.expanded.remove(&id);
                            self.move_to(self.sel as isize);
                        }
                        _ => {}
                    }
                }
                Vec::new()
            }
            Command::MovePageUp | Command::MovePageDown => {
                let page = body_rows.saturating_sub(2).max(1) as isize;
                let delta = if command == Command::MovePageUp { -page } else { page };
                if self.state.tab == PanelTab::Changes && self.diff_focused {
                    // The DIFF pages, not the file cursor: paging used to move
                    // the cursor and silently retarget `x`.
                    self.diff_scroll = (self.diff_scroll as isize + delta).max(0) as usize;
                } else {
                    self.move_to(self.sel as isize + delta);
                }
                Vec::new()
            }
            // 7. A digit jumps to that row AND affirms it — one gesture.
            Command::PanelPick => {
                let Some(digit) = digit.filter(|d| (1..=9).contains(d)) else {
                    return Vec::new();
                };
                match self.pick_target(digit, body_rows) {
                    Some(at) => {
                        self.sel = at;
                        self.confirm(at)
                    }
                    // Past the window: nothing. No clamp, no nearest-row.
                    None => Vec::new(),
                }
            }
            // 11. The changes verbs. A second `x` does NOT widen — the rail
            // teaches `x x` as "arm, then confirm", so the all-scope has its
            // own key and its own arm.
            Command::ChangesRevert => {
                if self.state.tab == PanelTab::Changes {
                    match self.items.get(self.sel) {
                        Some(item) => self.pending = Some(PendingRevert::File(item.clone())),
                        None => self.message = Some(NOTHING_TO_REVERT.to_string()),
                    }
                }
                Vec::new()
            }
            Command::ChangesRevertAll => {
                if self.state.tab == PanelTab::Changes && !self.items.is_empty() {
                    self.pending = Some(PendingRevert::All);
                }
                Vec::new()
            }
            // 3./4. The surgery verbs, refused out loud rather than faked.
            Command::TreeExtract => {
                if self.state.tab == PanelTab::Tree {
                    self.message = Some(match self.rows().get(self.sel) {
                        Some(ForestRow::Message { .. }) => NOT_WIRED_EXTRACT.to_string(),
                        _ => EXTRACT_NEEDS_A_TURN.to_string(),
                    });
                }
                Vec::new()
            }
            Command::TreeMoveInto => {
                if self.state.tab == PanelTab::Tree {
                    self.message = Some(match self.rows().get(self.sel) {
                        Some(ForestRow::Message { .. }) => NOT_WIRED_MOVE_INTO.to_string(),
                        _ => MOVE_NEEDS_A_TURN.to_string(),
                    });
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The row a digit names, resolved against the SAME window the tab paints.
    fn pick_target(&self, digit: usize, body_rows: usize) -> Option<usize> {
        match self.state.tab {
            PanelTab::Tree => {
                let rows = self.rows();
                let chrome = usize::from(self.message.is_some());
                let (start, shown) = forest_window(rows.len(), self.sel, body_rows, chrome);
                let at = start + digit - 1;
                (digit <= shown && at < rows.len()).then_some(at)
            }
            // changes/theme: none — a digit that jumped-and-affirmed would
            // revert a file you never saw.
            _ => None,
        }
    }

    /// What ⏎ affirms, per tab.
    pub fn confirm(&mut self, at: usize) -> Vec<HostRequest> {
        match self.state.tab {
            PanelTab::Tree => {
                let rows = self.rows();
                let Some(row) = rows.get(at) else { return Vec::new() };
                match selection_for(row, &self.threads) {
                    Selection::Open(id) => {
                        self.state.open = false;
                        vec![HostRequest::Open(id)]
                    }
                    Selection::Expand(id) => {
                        self.expanded.insert(id);
                        Vec::new()
                    }
                    Selection::Drill(id) => {
                        self.drilled.insert(id);
                        Vec::new()
                    }
                    Selection::Fork { .. } => {
                        self.message = Some(NOT_WIRED_FORK.to_string());
                        Vec::new()
                    }
                    Selection::None => Vec::new(),
                }
            }
            PanelTab::Changes => match self.pending.take() {
                Some(PendingRevert::All) => vec![HostRequest::Revert(None)],
                Some(PendingRevert::File(item)) => {
                    vec![HostRequest::Revert(Some(vec![item.file.path]))]
                }
                // ⏎ with nothing armed toggles the diff focus.
                None => {
                    if !self.items.is_empty() {
                        self.diff_focused = !self.diff_focused;
                    }
                    Vec::new()
                }
            },
            // ⏎ KEEPS what is painted: the baseline moves, so leaving no
            // longer reverts it, and the write goes out behind the paint.
            // The keeping itself is `reduce_panel`'s; only the WRITE is here,
            // because a pure reducer cannot post one.
            PanelTab::Theme => {
                self.state = self.reduce_with_theme(PanelAction::Confirm);
                let Some(state) = self.theme.as_ref().and_then(|p| p.baseline().cloned())
                else {
                    return Vec::new();
                };
                vec![HostRequest::SaveTheme(crate::theme::persist_request(&state))]
            }
            _ => Vec::new(),
        }
    }
}

/// The refusals, verbatim. Every one names the gesture that WOULD work.
pub const EXTRACT_NEEDS_A_TURN: &str =
    "e splits a conversation at a TURN — move onto one first";
pub const MOVE_NEEDS_A_TURN: &str = "m brings a TURN here — move onto one first";
pub const NOT_WIRED_EXTRACT: &str = "splitting a conversation is not wired into this client yet";
pub const NOT_WIRED_MOVE_INTO: &str = "bringing a turn here is not wired into this client yet";
pub const NOT_WIRED_FORK: &str = "forking a turn is not wired into this client yet";
pub const NOT_WIRED_SUMMARIZE: &str =
    "branching with a summary is not wired into this client yet";
pub const NOTHING_TO_REVERT: &str = "nothing to revert here";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forest::fixtures::{msg, session_row, with_origin};
    use bough_core::schema::parts::{Role, SessionKind};
    use serde_json::json;

    fn host() -> PanelHost {
        let mut h = PanelHost::default();
        h.sessions = vec![
            session_row("a", SessionKind::Root, 1),
            session_row("b", SessionKind::Root, 2),
        ];
        h.threads = HashMap::from([(
            "a".to_string(),
            vec![msg("m1", Role::User, "go"), msg("m2", Role::Supervisor, "done")],
        )]);
        h
    }

    fn changes_set(files: Vec<serde_json::Value>) -> SessionChangeSet {
        SessionChangeSet {
            available: true,
            reason: None,
            base: Some("abcdef1234".into()),
            files,
            workspace: Some("/tmp/x".into()),
        }
    }

    fn file(path: &str) -> serde_json::Value {
        json!({
            "path": path,
            "status": "modified",
            "hunks": [{"header": "@@ -1 +1 @@", "lines": ["-a", "+b"]}],
        })
    }

    #[test]
    fn opening_the_panel_lands_on_the_tree_and_asks_for_its_rows() {
        let mut h = host();
        let requests = h.handle(Command::PanelToggle, None, 10);
        assert!(h.open());
        assert_eq!(h.tab(), PanelTab::Tree);
        assert_eq!(requests, vec![HostRequest::LoadSessions]);
        // …and the second press closes it, asking for nothing.
        assert!(h.handle(Command::PanelToggle, None, 10).is_empty());
        assert!(!h.open());
    }

    #[test]
    fn the_switcher_lands_on_you_are_here_not_on_row_one() {
        let mut h = host();
        h.current_id = Some("a".into()); // `a` is older, so it sorts second
        h.handle(Command::PanelToggle, None, 10);
        assert_eq!(h.rows()[h.sel].id(), "a");
    }

    #[test]
    fn entering_the_changes_tab_fetches_it_and_leaving_clears_what_was_armed() {
        let mut h = host();
        h.handle(Command::PanelToggle, None, 10);
        let requests = h.handle(Command::Tab(PanelTab::Changes), None, 10);
        assert_eq!(requests, vec![HostRequest::LoadChanges]);
        h.set_changes(Some(changes_set(vec![file("a.ts")])));
        h.handle(Command::ChangesRevert, None, 10);
        assert!(h.pending.is_some());
        // Leaving drops the arm: a confirmation that outlives the row it was
        // read on is not a confirmation.
        h.handle(Command::Tab(PanelTab::Tree), None, 10);
        assert!(h.pending.is_none());
        assert!(!h.diff_focused);
    }

    #[test]
    fn the_two_press_revert_arms_then_confirms_and_x_never_widens_to_all() {
        let mut h = host();
        h.handle(Command::Tab(PanelTab::Changes), None, 10);
        h.set_changes(Some(changes_set(vec![file("a.ts"), file("b.ts")])));
        // First press arms THIS path…
        assert!(h.handle(Command::ChangesRevert, None, 10).is_empty());
        assert_eq!(h.pending, Some(PendingRevert::File(h.items[0].clone())));
        // …and a second `x` re-arms the same path rather than widening.
        h.handle(Command::ChangesRevert, None, 10);
        assert_eq!(h.pending, Some(PendingRevert::File(h.items[0].clone())));
        // ⏎ performs it, addressed to the path.
        assert_eq!(
            h.handle(Command::PanelConfirm, None, 10),
            vec![HostRequest::Revert(Some(vec!["a.ts".to_string()]))]
        );
        assert!(h.pending.is_none());

        // The all-scope is its own key, and `None` means the whole set — the
        // server refuses `[]`, so an empty list is never sent.
        h.handle(Command::ChangesRevertAll, None, 10);
        assert_eq!(h.pending, Some(PendingRevert::All));
        assert_eq!(
            h.handle(Command::PanelConfirm, None, 10),
            vec![HostRequest::Revert(None)]
        );
    }

    #[test]
    fn escape_unwinds_one_level_before_it_closes_the_panel() {
        let mut h = host();
        h.handle(Command::Tab(PanelTab::Changes), None, 10);
        h.set_changes(Some(changes_set(vec![file("a.ts")])));
        h.handle(Command::MoveIn, None, 10);
        h.handle(Command::ChangesRevert, None, 10);
        // pending revert → armed nothing → diff focus → the panel.
        h.handle(Command::PanelClose, None, 10);
        assert!(h.pending.is_none());
        assert!(h.diff_focused, "the focus must survive cancelling the revert");
        assert!(h.open());
        h.handle(Command::PanelClose, None, 10);
        assert!(!h.diff_focused);
        assert!(h.open());
        h.handle(Command::PanelClose, None, 10);
        assert!(!h.open());
    }

    #[test]
    fn the_cursor_moving_drops_the_arming() {
        let mut h = host();
        h.handle(Command::Tab(PanelTab::Changes), None, 10);
        h.set_changes(Some(changes_set(vec![file("a.ts"), file("b.ts")])));
        h.handle(Command::ChangesRevert, None, 10);
        h.handle(Command::MoveDown, None, 10);
        assert!(h.pending.is_none());
        assert_eq!(h.sel, 1);
        // …and the cursor never runs past the end.
        h.handle(Command::MoveDown, None, 10);
        assert_eq!(h.sel, 1);
    }

    #[test]
    fn a_focused_diff_takes_the_arrows_and_the_pages_from_the_file_cursor() {
        let mut h = host();
        h.handle(Command::Tab(PanelTab::Changes), None, 10);
        h.set_changes(Some(changes_set(vec![file("a.ts"), file("b.ts")])));
        h.handle(Command::MoveIn, None, 10);
        assert!(h.diff_focused);
        h.handle(Command::MoveDown, None, 10);
        assert_eq!(h.diff_scroll, 1);
        assert_eq!(h.sel, 0, "the file cursor must not move under a focused diff");
        h.handle(Command::MovePageDown, None, 10);
        assert_eq!(h.diff_scroll, 9);
        assert_eq!(h.sel, 0);
    }

    #[test]
    fn enter_on_a_conversation_opens_it_and_closes_the_panel() {
        let mut h = host();
        h.handle(Command::PanelToggle, None, 10);
        let requests = h.handle(Command::PanelConfirm, None, 10);
        // Newest first: `b`.
        assert_eq!(requests, vec![HostRequest::Open("b".to_string())]);
        assert!(!h.open(), "opening a conversation closes the panel");
    }

    #[test]
    fn right_expands_a_conversation_and_left_closes_it_again() {
        let mut h = host();
        h.handle(Command::PanelToggle, None, 10);
        h.move_to(1); // `a`, which has a thread
        h.handle(Command::MoveIn, None, 10);
        assert!(h.expanded.contains("a"));
        assert!(h.rows().iter().any(|r| r.id() == "m1"));
        // From a TURN, ← closes the conversation it belongs to.
        h.move_to(2);
        h.handle(Command::MoveOut, None, 10);
        assert!(!h.expanded.contains("a"));
    }

    #[test]
    fn a_collapsed_fan_out_drills_in_rather_than_opening_anything() {
        let mut h = PanelHost::default();
        let root = session_row("root", SessionKind::Root, 1);
        let sub = with_origin(session_row("sub", SessionKind::Subagent, 2), "root");
        h.sessions = vec![root, sub];
        h.expanded.insert("root".into());
        h.threads.insert("root".into(), vec![]);
        h.handle(Command::PanelToggle, None, 10);
        let collapsed =
            h.rows().iter().position(|r| matches!(r, ForestRow::Collapsed { .. })).unwrap();
        h.move_to(collapsed as isize);
        assert!(h.handle(Command::PanelConfirm, None, 10).is_empty());
        assert!(h.drilled.contains("root"));
        assert!(h.rows().iter().any(|r| r.id() == "sub"));
    }

    #[test]
    fn a_digit_jumps_to_that_row_and_affirms_it_in_one_gesture() {
        let mut h = host();
        h.handle(Command::PanelToggle, None, 10);
        // Row 2 of the window is `a` (newest first: b, a).
        assert_eq!(h.handle(Command::PanelPick, Some(2), 10), vec![HostRequest::Open("a".into())]);
        // A digit past the window does nothing — no clamp, no nearest row.
        let mut h = host();
        h.handle(Command::PanelToggle, None, 10);
        assert!(h.handle(Command::PanelPick, Some(9), 10).is_empty());
        assert!(h.open());
    }

    #[test]
    fn the_surgery_verbs_refuse_out_loud_and_name_the_gesture_that_would_work() {
        let mut h = host();
        h.handle(Command::PanelToggle, None, 10);
        // On a conversation row: `e` says what it needs.
        h.handle(Command::TreeExtract, None, 10);
        assert_eq!(h.message.as_deref(), Some(EXTRACT_NEEDS_A_TURN));
        h.handle(Command::TreeMoveInto, None, 10);
        assert_eq!(h.message.as_deref(), Some(MOVE_NEEDS_A_TURN));
        // On a turn: the honest "not wired yet".
        h.expanded.insert("a".into());
        let turn = h.rows().iter().position(|r| r.id() == "m1").unwrap();
        h.move_to(turn as isize);
        h.handle(Command::TreeExtract, None, 10);
        assert_eq!(h.message.as_deref(), Some(NOT_WIRED_EXTRACT));
        // ⏎ on a turn is a fork, and says so rather than doing nothing.
        h.handle(Command::PanelConfirm, None, 10);
        assert_eq!(h.message.as_deref(), Some(NOT_WIRED_FORK));
    }

    #[test]
    fn summarize_fork_acts_on_the_tree_and_nowhere_else() {
        let mut h = host();
        h.handle(Command::Tab(PanelTab::Changes), None, 10);
        h.handle(Command::PanelConfirmSummarize, None, 10);
        assert_eq!(h.message, None, "`s` outside the tree must not affirm anything");
        h.handle(Command::Tab(PanelTab::Tree), None, 10);
        h.handle(Command::PanelConfirmSummarize, None, 10);
        assert_eq!(h.message.as_deref(), Some(NOT_WIRED_SUMMARIZE));
    }

    #[test]
    fn the_reveal_path_seeds_the_expansion_so_the_open_conversation_is_on_screen() {
        let mut h = PanelHost::default();
        h.current_id = Some("hand".into());
        let root = session_row("root", SessionKind::Root, 1);
        let hand = with_origin(session_row("hand", SessionKind::Root, 2), "root");
        h.handle(Command::PanelToggle, None, 10);
        h.set_sessions(vec![root, hand]);
        assert!(h.expanded.contains("root"), "the origin must be opened to reach the current row");
        assert_eq!(h.rows()[h.sel].id(), "hand");
    }

    // ---- the theme tab (Theme.tsx's half of PanelHost.tsx) ------------------

    /// A host already on the theme tab, with a preview that paints NOWHERE —
    /// `ThemePreview::new` would drive the process-global palette, and a unit
    /// test that repaints the terminal is a test that fails under `--jobs`.
    fn themed() -> PanelHost {
        let mut h = PanelHost::default();
        h.handle(Command::Tab(PanelTab::Theme), None, 10);
        h.theme =
            Some(crate::theme::ThemePreview::with_apply(None, Box::new(|_| {})));
        h
    }

    #[test]
    fn entering_the_theme_tab_asks_for_the_palette_in_force() {
        let mut h = PanelHost::default();
        let requests = h.handle(Command::Tab(PanelTab::Theme), None, 10);
        assert_eq!(h.tab(), PanelTab::Theme);
        assert_eq!(requests, vec![HostRequest::LoadTheme]);
    }

    #[test]
    fn the_answer_seeds_the_baseline_and_re_entering_does_not_re_seed_it() {
        let mut h = PanelHost::default();
        h.handle(Command::Tab(PanelTab::Theme), None, 10);
        let iris = crate::theme::state_for(None, &crate::theme::THEME_PRESETS[2]);
        h.set_theme(Some(iris.clone()));
        assert_eq!(h.theme.as_ref().unwrap().baseline(), Some(&iris));
        // A second answer (a re-entry's fetch) must NOT move the baseline: it
        // would adopt a palette the user is only previewing.
        h.set_theme(Some(crate::theme::ThemeState::default()));
        assert_eq!(h.theme.as_ref().unwrap().baseline(), Some(&iris));
    }

    #[test]
    fn moving_previews_and_leaves_the_panels_own_cursor_alone() {
        let mut h = themed();
        h.handle(Command::MoveDown, None, 10);
        h.handle(Command::MoveDown, None, 10);
        assert_eq!(h.theme.as_ref().unwrap().index(), 2);
        assert!(h.theme.as_ref().unwrap().previewing());
        // The preview owns the cursor; `sel` advancing beside it would make a
        // digit affirm a row the picker is not on.
        assert_eq!(h.sel, 0);
    }

    #[test]
    fn leaving_the_theme_tab_reverts_what_was_only_being_previewed() {
        // All five exits, because a revert remembered at four of them is a
        // picker that silently keeps the theme you last scrolled past.
        for leave in [
            Command::PanelClose,
            Command::PanelToggle,
            Command::Tab(PanelTab::Tree),
            Command::PanelNext,
            Command::PanelPrev,
        ] {
            let mut h = themed();
            h.handle(Command::MoveDown, None, 10);
            assert!(h.theme.as_ref().unwrap().previewing(), "{leave:?}");
            h.handle(leave, None, 10);
            let preview = h.theme.as_ref().unwrap();
            assert!(!preview.previewing(), "{leave:?} must revert");
            assert_eq!(preview.index(), 0, "{leave:?} must restore the baseline row");
        }
    }

    #[test]
    fn keeping_a_palette_persists_it_and_then_leaving_no_longer_reverts() {
        let mut h = themed();
        h.handle(Command::MoveDown, None, 10); // → Fjord
        let requests = h.handle(Command::PanelConfirm, None, 10);
        assert_eq!(
            requests,
            vec![HostRequest::SaveTheme(crate::theme::ThemeWrite::Put {
                name: "Fjord".into(),
                colors: crate::theme::THEME_PRESETS[1].colors_map(),
            })]
        );
        h.handle(Command::PanelClose, None, 10);
        assert_eq!(h.theme.as_ref().unwrap().index(), 1, "a kept palette survives leaving");
    }

    #[test]
    fn keeping_the_default_persists_as_a_delete_not_an_empty_put() {
        let mut h = themed();
        h.handle(Command::MoveDown, None, 10);
        h.handle(Command::MoveUp, None, 10); // back onto Default
        let requests = h.handle(Command::PanelConfirm, None, 10);
        assert_eq!(
            requests,
            vec![HostRequest::SaveTheme(crate::theme::ThemeWrite::Delete)],
            "a PUT of an empty map stores a NAMED theme overriding nothing"
        );
    }

    #[test]
    fn a_digit_never_affirms_a_palette() {
        let mut h = themed();
        let requests = h.handle(Command::PanelPick, Some(3), 10);
        assert!(requests.is_empty());
        assert_eq!(h.theme.as_ref().unwrap().index(), 0, "1-9 is not a theme gesture");
    }
}
