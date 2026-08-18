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
use bough_core::schema::requests::PartPick;

use crate::api::{
    McpStatus, ModelRow, SessionRow, SkillSourceRow, WorkflowDetail, WorkflowSummary,
};
use crate::forest::{forest_rows, reveal_path, selection_for, ForestInput, ForestRow, Selection};
use crate::keys::{Command, PanelTab};
use crate::store::state::SessionChangeSet;

use super::changes::{change_items, ChangeItem, PendingRevert};
use super::mcp::mcp_names;
use super::model::{
    display_rows, model_entries, model_window, visible_entries, ModelConfig, ModelEntry,
    ModelFilters, Tier,
};
use super::skills::SkillRow;
use super::tree::forest_window;
use super::workflows::{phase_groups, visible_agents, wf_runs_height, WfLevel, WF_FILTERS};
use super::{panel_action_for, PanelAction, PanelState, INITIAL_PANEL};

/// What the panel needs the app to do. Every one is a REST call the loop owns;
/// the host itself performs no I/O.
// Not `Eq`: the surgery verbs carry `PartPick`, which is a wire body and only
// `PartialEq`. Nothing compares these for identity.
#[derive(Clone, Debug, PartialEq)]
pub enum HostRequest {
    /// `GET /sessions` — the tree's rows, on entry.
    LoadSessions,
    /// `GET /sessions/:id/changes` — the change set, on entry.
    LoadChanges,
    /// Open this conversation and close the panel.
    Open(String),
    /// `GET /sessions/:id` — the turns of a conversation that is NOT the open
    /// one, asked for the first time its row is expanded. Lazy and deduped:
    /// `panel.threads` was only ever filled from the open session, so every
    /// other row expanded to nothing and ⏎-fork / `e` / `m` were unreachable
    /// there.
    LoadThread(String),
    /// `GET /sessions?originId=:id` — the drill-in, asked the first time a row
    /// is expanded. The plain listing excludes the collapsing kinds.
    LoadChildSessions(String),
    /// `POST /sessions/:id/changes/revert`. `None` = the whole set; the server
    /// refuses `[]`, so an empty list is never sent.
    Revert(Option<Vec<String>>),
    /// `GET /theme` — the palette in force, on entry to the theme tab.
    LoadTheme,
    /// Persist the kept palette. The verb is decided by
    /// `theme::persist_request`, never here.
    SaveTheme(crate::theme::ThemeWrite),
    // ---- the workflows tab -------------------------------------------------
    /// `GET /workflows?session=` — the run list, on entry.
    LoadWorkflows,
    /// `GET /workflows/:id` — one run's whole view, on open and on refresh.
    LoadWorkflow(String),
    /// `POST /workflows/:id/{pause,resume,stop,rerun}`.
    SteerWorkflow { id: String, action: WorkflowAction },
    /// `POST /workflows/:id/save` — store the script to run again by name.
    SaveWorkflow(String),
    // ---- the mcp tab -------------------------------------------------------
    /// `GET /mcp/servers` — RE-FETCHED on every entry, never cached.
    LoadMcp,
    /// `POST /mcp/servers/:name/{enable,disable}` — the grant.
    SetMcpEnabled { name: String, enabled: bool },
    /// `PUT /mcp/servers/:name` — register a remote server by URL.
    AddMcpServer { name: String, url: String },
    /// `DELETE /mcp/servers/:name` — drop the registration itself.
    DeleteMcpServer(String),
    /// `POST /mcp/servers/:name/connect` — the `c` test.
    ConnectMcpServer(String),
    /// `POST /mcp/servers/:name/restart`.
    RestartMcpServer(String),
    /// `POST /mcp/servers/:name/auth` — begin the flow; the answer carries the
    /// URL the tab prints.
    BeginMcpAuth(String),
    /// `DELETE /mcp/servers/:name/auth` — forget the stored credentials.
    ClearMcpAuth(String),
    // ---- the skills tab ----------------------------------------------------
    /// `GET /skills` — the listing AND the directories that were walked.
    LoadSkillRows,
    /// `GET /hooks` — the hooks tab's rows.
    LoadHooks,
    /// The context tab's one fetch.
    LoadPrompt,
    /// `POST /hooks/:name` — turn one on or off, then reload the list.
    ToggleHook { name: String, enabled: bool },
    /// `GET /plugins` — the plugins tab's rows.
    LoadPlugins,
    /// `POST /plugins/:id` — turn one plugin, or one thing inside one, on or
    /// off, then reload the list.
    TogglePlugin { id: String, enabled: bool },
    // ---- the model tab -----------------------------------------------------
    /// `GET /models` — the catalog, answered server-side because the server is
    /// the process that holds the credential.
    LoadModels,
    /// `GET /model-settings` — what a NEW conversation runs on, both tiers.
    LoadModelSettings,
    /// `PUT /model-settings` + the session pin. The whole config travels, so a
    /// caller cannot perform half of spec §12's "pins this session AND moves
    /// the default".
    SaveModel(ModelConfig),
    /// Open this agent's backing session (`o` in the workflows tab).
    OpenAgentSession(String),
    // ---- the tree's surgery verbs ------------------------------------------
    /// `POST /sessions/:id/fork` — ⏎ on a turn. Addressed to the ROW's own
    /// conversation, never to the open one: the forest shows every
    /// conversation's turns, so ⏎ on a branch's turn branches that branch.
    /// `editor_text` is a user turn's own text, which goes back to the composer
    /// so the re-send IS the new branch.
    Fork {
        session_id: String,
        at_message_id: String,
        exclusive: bool,
        summarize_abandoned: bool,
        editor_text: Option<String>,
    },
    /// `POST /sessions/:id/extract` — `e`. The turn under the cursor and every
    /// later turn of ITS conversation become a fresh root, and it opens.
    Extract {
        session_id: String,
        picks: Vec<PartPick>,
    },
    /// `POST /sessions/:id/move-into` — `m`, extract's mirror: the same turns
    /// copied onto the tail of the conversation that is OPEN.
    MoveInto {
        target_id: String,
        source_id: String,
        picks: Vec<PartPick>,
    },
}

/// Where the tree's arrival puts the cursor: on the open conversation (the
/// switcher's "you are here") or on the turn a rewind is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Land {
    Current,
    Rewind,
}

/// The four steering verbs, so the request carries one enum rather than a
/// stringly-typed path fragment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowAction {
    Pause,
    Resume,
    Stop,
    Rerun,
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
    /// The drill-in rows, per origin id. `GET /sessions` DELIBERATELY excludes
    /// the collapsing kinds (subagent, workflow_agent, schedule_run) — they
    /// surface only via `GET /sessions?originId=`. Without this map every such
    /// row is invisible on every surface at once: no rail row while it runs, no
    /// branch card when it finishes, no node in the tree.
    pub children_by_origin: HashMap<String, Vec<SessionRow>>,
    pub threads: HashMap<String, Vec<Message>>,
    pub expanded: HashSet<String>,
    pub drilled: HashSet<String>,
    /// Turns whose tool calls are shown as rows. Keyed by MESSAGE id, so
    /// unfolding a turn in one conversation says nothing about any other.
    pub tools_open: HashSet<String>,
    /// The re-rooted views walked into, outermost first. Empty = the forest.
    /// A STACK rather than one id, because `esc` has to walk back out the way
    /// the reader walked in.
    pub root_stack: Vec<String>,
    pub current_id: Option<String>,
    /// Where the cursor should land when the tree's arrival fetch answers, and
    /// on THAT answer only. `None` = the listing is a refresh and the cursor
    /// belongs to whoever moved it last (the user).
    land: Option<Land>,
    pub workspace: Option<String>,
    /// The `/` buffer. In the tree it is a FULL-TEXT search of every message,
    /// which is what the keymap has always claimed it was.
    pub filter: String,
    /// Does the buffer have the text keyboard? (`panelFiltering`.)
    pub filtering: bool,
    /// Conversations whose MESSAGES matched, from `GET /search` — a row is
    /// either reachable or it is not, and that is what the switcher is asking.
    pub matched_sessions: Vec<String>,
    /// The matched message ids, so the row that said the word is marked.
    pub matched_messages: Vec<String>,
    /// Topic headers per conversation, from `POST /sessions/:id/sections`.
    /// Absent = not fetched, which is NOT the same as "no topics".
    pub sections: HashMap<String, Vec<crate::forest::SectionRange>>,
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
    // ---- the workflows tab -------------------------------------------------
    pub runs: Vec<WorkflowSummary>,
    /// `GET /workflows/:id` for the opened run. `None` at level 0, and while
    /// the fetch is in flight — the view falls back to the run list rather
    /// than painting a header full of zeroes.
    pub run_detail: Option<WorkflowDetail>,
    /// 0 runs · 1 phases · 2 a phase's agents · 3 one agent · 4 the script.
    pub wf_level: WfLevel,
    pub phase_sel: usize,
    pub agent_sel: usize,
    pub wf_scroll: usize,
    /// Index into [`WF_FILTERS`]; `f` cycles it.
    pub wf_filter: usize,
    pub prompt_open: bool,
    /// The run's last log line, for the header's `▸` row.
    pub last_log: Option<String>,
    // ---- the mcp tab -------------------------------------------------------
    /// NEVER CACHED: re-fetched on every entry, because grants and connections
    /// change between turns and a panel showing last minute's MCP state is
    /// worse than one showing none.
    pub mcp: Option<McpStatus>,
    /// The server URL being typed (`n`), or `None` when the buffer is closed.
    pub mcp_entry: Option<String>,
    /// A registration `d` armed. A second `d` performs it.
    pub mcp_pending_delete: Option<String>,
    // ---- the skills tab ----------------------------------------------------
    /// `None` = nothing has answered yet. Never rendered as "no skills
    /// installed", which is a claim about a directory this client never read.
    pub skills: Option<Vec<SkillRow>>,
    pub skill_sources: Vec<SkillSourceRow>,
    /// Why the listing is absent, when it is.
    pub skills_note: Option<String>,
    /// `None` = `GET /hooks` has not answered. Never rendered as an empty
    /// directory — see `panel/hooks.rs`.
    pub hooks: Option<Vec<crate::components::panel::hooks::HookRow>>,
    pub hooks_dir: Option<String>,
    pub hooks_note: Option<String>,
    /// `None` = `GET /plugins` has not answered. Never rendered as an empty
    /// directory — see `panel/plugins.rs`.
    pub plugins: Option<Vec<crate::components::panel::plugins::PluginGroupRow>>,
    pub plugins_dir: Option<String>,
    pub plugins_note: Option<String>,
    /// `None` = `GET /sessions/:id/prompt` has not answered. Rendered as an
    /// absence, never as a prompt with nothing in it.
    pub prompt: Option<crate::api::PromptView>,
    pub prompt_note: Option<String>,
    // ---- the model tab -----------------------------------------------------
    pub models: Vec<ModelRow>,
    pub model_cfg: ModelConfig,
    pub model_filters: ModelFilters,
    /// Which search box has the keyboard, or `None` when neither does.
    pub model_focus: Option<Tier>,
    /// The tab was opened and the cursor has not been moved since — the next
    /// fetch to arrive should land it on the model in force.
    pub model_land: bool,
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
            children_by_origin: HashMap::new(),
            drilled: HashSet::new(),
            tools_open: HashSet::new(),
            root_stack: Vec::new(),
            current_id: None,
            land: None,
            workspace: None,
            filter: String::new(),
            filtering: false,
            matched_sessions: Vec::new(),
            matched_messages: Vec::new(),
            sections: HashMap::new(),
            changes: None,
            items: Vec::new(),
            diff_focused: false,
            diff_scroll: 0,
            pending: None,
            theme: None,
            runs: Vec::new(),
            run_detail: None,
            wf_level: 0,
            phase_sel: 0,
            agent_sel: 0,
            wf_scroll: 0,
            wf_filter: 0,
            prompt_open: false,
            last_log: None,
            mcp: None,
            mcp_entry: None,
            mcp_pending_delete: None,
            skills: None,
            skill_sources: Vec::new(),
            skills_note: None,
            hooks: None,
            hooks_dir: None,
            hooks_note: None,
            plugins: None,
            plugins_dir: None,
            plugins_note: None,
            prompt: None,
            prompt_note: None,
            models: Vec::new(),
            model_cfg: ModelConfig::default(),
            model_filters: ModelFilters::default(),
            model_focus: None,
            model_land: false,
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
        forest_rows(&ForestInput {
            sessions: &self.sessions,
            children_by_origin: &self.children_by_origin,
            threads: &self.threads,
            expanded: &self.expanded,
            drilled: &self.drilled,
            tools_open: &self.tools_open,
            root_id: self.root_stack.last().map(String::as_str),
            current_id: self.current_id.as_deref(),
            // Only when the buffer belongs to THIS tab: an MCP URL half-typed
            // must never narrow the conversation list underneath it.
            filter: (self.state.tab == PanelTab::Tree && !self.filter.is_empty())
                .then_some(self.filter.as_str()),
            matched_sessions: &self.matched_sessions,
            matched_messages: &self.matched_messages,
            sections: Some(&self.sections),
            ..Default::default()
        })
    }

    /// The re-rooted view's lineage, as titles. Empty when the tree is showing
    /// the whole forest, which is what makes the crumb row conditional.
    pub fn crumbs(&self) -> Vec<String> {
        self.root_stack
            .iter()
            .map(|id| {
                self.sessions
                    .iter()
                    .chain(self.children_by_origin.values().flatten())
                    .find(|s| &s.session.id == id)
                    .map(super::tree::title_of)
                    // A branch whose row scrolled out of the fetched set still
                    // gets a crumb: losing the trail is worse than a dull name.
                    .unwrap_or_else(|| "branch".to_string())
            })
            .collect()
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
        for id in reveal_path(
            &sessions,
            &self.children_by_origin,
            self.current_id.as_deref(),
        ) {
            self.expanded.insert(id);
        }
        self.sessions = sessions;
        // ONCE, on the listing the ARRIVAL asked for — never on every refresh.
        //
        // The tree re-reads `GET /sessions` on the rail's beat, and landing on
        // every answer re-parked the cursor about once a second: ↓ moved a row
        // and the next poll pulled it straight back to "you are here", which
        // made every verb below (⏎ to fork, `e`, `m`) unreachable by hand.
        // Arrival is the moment the landing is about, and `take` is what makes
        // it that moment only.
        match self.land.take() {
            Some(Land::Current) => self.land_on_current(),
            Some(Land::Rewind) => {
                self.sel = crate::forest::rewind_index(&self.rows(), self.current_id.as_deref());
            }
            None => {}
        }
    }

    /// The drill-in answer for one origin. Replaces that origin's list only —
    /// the plain listing and every OTHER origin's children are untouched, so a
    /// poll for the open conversation cannot erase what an expanded row fetched.
    pub fn set_children(&mut self, origin_id: String, rows: Vec<SessionRow>) {
        // An EMPTY answer is stored, not dropped: "nothing branched from it" is
        // a fact worth remembering, and it is what stops the expand from
        // re-asking on every keypress.
        self.children_by_origin.insert(origin_id, rows);
    }

    /// What expanding a row must fetch, ONCE. Both answers land in maps keyed
    /// by the id, so "have we asked" is "is the key present" — no request is
    /// repeated while the row stays open, and nothing is fetched for rows
    /// nobody expanded (the N+1 the tree would otherwise pay on every poll).
    fn fetch_on_expand(&self, id: &str) -> Vec<HostRequest> {
        let mut out = Vec::new();
        if !self.threads.contains_key(id) {
            out.push(HostRequest::LoadThread(id.to_string()));
        }
        if !self.children_by_origin.contains_key(id) {
            out.push(HostRequest::LoadChildSessions(id.to_string()));
        }
        out
    }

    /// The switcher lands on you-are-here.
    fn land_on_current(&mut self) {
        let rows = self.rows();
        // A conversation that branched off another is a BRANCH row, not a
        // session row — looking only for the latter landed "you are here" on
        // the root of every forked conversation.
        let current = self.current_id.as_deref();
        self.sel = rows
            .iter()
            .position(|r| match r {
                ForestRow::Session { current: true, .. } => true,
                ForestRow::Branch { id, .. } => Some(id.as_str()) == current,
                _ => false,
            })
            .unwrap_or(0);
    }

    /// Open the workflows tab ON one run — the rail's ⏎ on a workflow row.
    ///
    /// The TS opens `unit.sessionId`, which for a run IS THE RUN ID (the rail
    /// builds it that way), so it asked `GET /sessions/<run id>` and the row
    /// did nothing but print a 404. A run's surface is the workflows tab, and
    /// this lands on it drilled in, which is what the row is about.
    pub fn open_run(&mut self, id: &str) -> Vec<HostRequest> {
        self.state.open = true;
        self.state.tab = PanelTab::Workflows;
        self.wf_level = 1;
        self.phase_sel = 0;
        self.agent_sel = 0;
        self.wf_scroll = 0;
        self.prompt_open = false;
        self.message = None;
        vec![
            HostRequest::LoadWorkflows,
            HostRequest::LoadWorkflow(id.to_string()),
        ]
    }

    /// The row under the cursor, as `(message id, its conversation)` — `None`
    /// for anything that is not a TURN, which is what both `e` and `m` refuse.
    fn turn_row(&self) -> Option<(String, String)> {
        match self.rows().get(self.sel) {
            Some(ForestRow::Message { id, session_id, .. }) => {
                Some((id.clone(), session_id.clone()))
            }
            _ => None,
        }
    }

    /// That turn and every LATER turn of its own conversation — what both `e`
    /// and `m` copy. `None` when the turn is no longer in the thread the tree
    /// was built from, which is a stale row rather than an error.
    fn picks_from(&self, session_id: &str, at_message_id: &str) -> Option<Vec<PartPick>> {
        let thread = self.threads.get(session_id)?;
        let at = thread.iter().position(|m| m.id == at_message_id)?;
        Some(
            thread[at..]
                .iter()
                .map(|m| PartPick {
                    message_id: m.id.clone(),
                    parts: None,
                })
                .collect(),
        )
    }

    /// `esc esc` — the tree, opened ON the turn you would go back to.
    ///
    /// The landing row is the entire difference between this and `^t`: the open
    /// conversation is EXPANDED (so its turns are rows at all) and the cursor is
    /// put on its last user turn, where ⏎ means "edit this and branch".
    /// Arriving at the top of a forest of forty conversations would make the
    /// commonest correction in the product a scroll.
    fn rewind(&mut self) -> Vec<HostRequest> {
        let id = self.current_id.clone();
        if let Some(id) = &id {
            self.expanded.insert(id.clone());
        }
        let was_open = self.state.open && self.state.tab == PanelTab::Tree;
        self.state.open = true;
        self.state.tab = PanelTab::Tree;
        // Computed AFTER the expand, against the rows as they will be WITH this
        // conversation open: the index is meaningless against the collapsed
        // forest, where the turn is not a row at all.
        self.sel = crate::forest::rewind_index(&self.rows(), id.as_deref());
        // The arrival fetch still runs when the tree was NOT already open — and
        // `arrive` resets the cursor AND the message, so both are restored
        // after it.
        // The tree may not have its rows yet on a cold open — park the intent
        // so the arrival's own listing lands on the turn rather than on the
        // switcher's row.
        self.land = Some(Land::Rewind);
        let requests = if was_open {
            Vec::new()
        } else {
            let sel = self.sel;
            let requests = self.arrive(PanelTab::Tree);
            self.sel = sel;
            // `arrive` parked its own; the rewind's outranks it.
            self.land = Some(Land::Rewind);
            requests
        };
        if id.is_none() {
            self.message = Some(REWIND_NEEDS_A_CONVERSATION.to_string());
        }
        requests
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
                // Now, against the rows already in hand, AND again when the
                // arrival fetch answers — the listing is what makes the row
                // exist, and on a cold open there is nothing here to land on.
                self.land = Some(Land::Current);
                self.land_on_current();
                vec![HostRequest::LoadSessions]
            }
            // Nothing to fetch: the recap is derived from the thread the
            // transcript is already holding.
            PanelTab::Recap => vec![],
            PanelTab::Changes => vec![HostRequest::LoadChanges],
            // The cursor is the PREVIEW's, not the panel's: it starts on the
            // theme in force, so `sel` is left at 0 and never read here.
            PanelTab::Theme => vec![HostRequest::LoadTheme],
            // A run view opens on the LIST, never on whatever run was last
            // drilled into: a level that outlives its tab shows a run's header
            // over another conversation's list.
            PanelTab::Workflows => {
                self.wf_level = 0;
                self.run_detail = None;
                self.phase_sel = 0;
                self.agent_sel = 0;
                self.wf_scroll = 0;
                self.prompt_open = false;
                vec![HostRequest::LoadWorkflows]
            }
            // NOTHING HERE IS CACHED. Grants and connections change between
            // turns, and a panel showing last minute's MCP state is worse than
            // one showing none.
            PanelTab::Mcp => {
                self.mcp_entry = None;
                self.mcp_pending_delete = None;
                vec![HostRequest::LoadMcp]
            }
            PanelTab::Skills => vec![HostRequest::LoadSkillRows],
            PanelTab::Hooks => vec![HostRequest::LoadHooks],
            PanelTab::Plugins => vec![HostRequest::LoadPlugins],
            // Re-fetched on every arrival: the shape describes the LAST turn,
            // so a tab opened after another turn ran must not show the one
            // before it.
            PanelTab::Context => vec![HostRequest::LoadPrompt],
            // Two fetches, because they answer two different questions: what
            // this install can route to, and what it is set to right now.
            //
            // The cursor lands on the model IN FORCE, not on row 0. The catalog
            // runs to hundreds of rows and the ● is wherever the active one
            // sorts — so a tab that opened at the top answered "which model is
            // this?" with a screenful of models that were not it, and the only
            // way to see the answer was to scroll looking for a dot. Landing is
            // deferred to the fetches: neither list is in hand yet here.
            PanelTab::Model => {
                self.model_focus = None;
                self.model_land = true;
                vec![HostRequest::LoadModels, HostRequest::LoadModelSettings]
            }
        }
    }

    // ---- what the fetches answer with --------------------------------------

    pub fn set_workflows(&mut self, runs: Vec<WorkflowSummary>) {
        self.runs = runs;
        if self.state.tab == PanelTab::Workflows && self.wf_level == 0 {
            self.sel = self.sel.min(self.runs.len().saturating_sub(1));
        }
    }

    /// One run's detail. Clamps the two pane cursors, because a refresh that
    /// arrives after an agent settles can shorten the list under them.
    pub fn set_workflow_detail(&mut self, detail: Option<WorkflowDetail>) {
        self.run_detail = detail;
        let Some(detail) = &self.run_detail else {
            return;
        };
        let groups = phase_groups(&detail.workflow, &detail.agents);
        self.phase_sel = self.phase_sel.min(groups.len().saturating_sub(1));
        let shown = groups
            .get(self.phase_sel)
            .map(|g| visible_agents(&g.agents, self.wf_filter()).len())
            .unwrap_or(0);
        self.agent_sel = self.agent_sel.min(shown.saturating_sub(1));
    }

    pub fn set_mcp(&mut self, status: Option<McpStatus>) {
        self.mcp = status;
        let count = self.mcp.as_ref().map(|s| mcp_names(s).len()).unwrap_or(0);
        self.sel = self.sel.min(count.saturating_sub(1));
    }

    /// The skills listing AND the directories that were walked. `None` is "the
    /// fetch failed" and carries its reason; it is never an empty list, which
    /// would be a claim about the user's `~/.bough/skills`.
    /// `GET /hooks` answered. `None` rows carry the reason in `note`.
    pub fn set_hooks(
        &mut self,
        hooks: Option<Vec<crate::components::panel::hooks::HookRow>>,
        dir: Option<String>,
        note: Option<String>,
    ) {
        // The cursor is clamped rather than reset: a toggle re-fetches the
        // whole list, and jumping back to the top after every space is the
        // kind of thing that makes a list unusable at ten rows.
        if let Some(rows) = &hooks {
            self.sel = self.sel.min(rows.len().saturating_sub(1));
        }
        self.hooks = hooks;
        if dir.is_some() {
            self.hooks_dir = dir;
        }
        self.hooks_note = note;
    }

    /// The hook the cursor is on, for the toggle key.
    pub fn selected_hook(&self) -> Option<&crate::components::panel::hooks::HookRow> {
        self.hooks.as_ref()?.get(self.sel)
    }

    /// `GET /plugins` answered. `None` rows carry the reason in `note`.
    pub fn set_plugins(
        &mut self,
        plugins: Option<Vec<crate::components::panel::plugins::PluginGroupRow>>,
        dir: Option<String>,
        note: Option<String>,
    ) {
        // Clamped rather than reset, for the reason the hooks setter gives: a
        // toggle re-fetches the whole list, and a cursor that jumped to the top
        // after every ⏎ would make the list unusable.
        if let Some(groups) = &plugins {
            let rows = crate::components::panel::plugins::plugin_rows(groups).len();
            self.sel = self.sel.min(rows.saturating_sub(1));
        }
        self.plugins = plugins;
        if dir.is_some() {
            self.plugins_dir = dir;
        }
        self.plugins_note = note;
    }

    /// The flat, cursor-addressable rows of the plugins tab.
    pub fn plugin_rows(&self) -> Vec<crate::components::panel::plugins::PluginRow> {
        self.plugins
            .as_ref()
            .map(|g| crate::components::panel::plugins::plugin_rows(g))
            .unwrap_or_default()
    }

    pub fn set_skills(
        &mut self,
        skills: Option<Vec<SkillRow>>,
        sources: Vec<SkillSourceRow>,
        note: Option<String>,
    ) {
        self.skills = skills;
        self.skill_sources = sources;
        self.skills_note = note;
        let count = self.filtered_skills().len();
        self.sel = self.sel.min(count.saturating_sub(1));
    }

    pub fn set_models(&mut self, models: Vec<ModelRow>) {
        self.models = models;
        self.land_on_active_model();
    }

    pub fn set_model_config(&mut self, cfg: ModelConfig) {
        self.model_cfg = cfg;
        self.land_on_active_model();
    }

    /// Put the cursor on the frontier model in force, while the tab is still
    /// showing what it opened with.
    ///
    /// Runs on BOTH arrivals rather than on whichever is expected to be last:
    /// the catalog says which rows exist and the settings say which one is
    /// active, and landing needs both. Whichever answers second is the one that
    /// gets it right, and the first is a harmless no-op.
    ///
    /// The arming is dropped by the first cursor move, so a slow `GET
    /// /model-settings` cannot yank the cursor out from under someone who has
    /// already started scrolling.
    fn land_on_active_model(&mut self) {
        if !self.model_land || self.state.tab != PanelTab::Model {
            return;
        }
        let entries = self.model_entries();
        if let Some(at) = entries
            .iter()
            .position(|e| e.tier() == Tier::Frontier && super::model::is_active(&self.model_cfg, e))
        {
            self.sel = at;
        }
    }

    // ---- derived lists, shared by the renderer and the cursor ---------------

    /// The filter in force for this tab, or `None`. A `/` buffer belongs to ONE
    /// tab: an MCP URL half-typed must never narrow the skills list underneath.
    fn skills_filter(&self) -> &str {
        if self.state.tab == PanelTab::Skills {
            self.filter.trim()
        } else {
            ""
        }
    }

    /// The skills the tab paints — the SAME list the cursor and the digits
    /// address. Two derivations of "which rows are visible" is how a digit comes
    /// to select a row nobody can see.
    pub fn filtered_skills(&self) -> Vec<SkillRow> {
        let Some(skills) = &self.skills else {
            return Vec::new();
        };
        let q = self.skills_filter().to_lowercase();
        if q.is_empty() {
            return skills.clone();
        }
        skills
            .iter()
            .filter(|s| {
                s.name.to_lowercase().contains(&q) || s.description.to_lowercase().contains(&q)
            })
            .cloned()
            .collect()
    }

    /// The `f` cycle's current value.
    pub fn wf_filter(&self) -> Option<&'static str> {
        WF_FILTERS[self.wf_filter % WF_FILTERS.len()]
    }

    /// The picker's flat entry list, narrowed per tier by its own box.
    pub fn model_entries(&self) -> Vec<ModelEntry> {
        model_entries(&self.models, None, &self.model_filters)
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
        // The URL buffer and the armed delete are nearer than the run level.
        if self.state.tab == PanelTab::Mcp {
            if self.mcp_entry.take().is_some() {
                return true;
            }
            if self.mcp_pending_delete.take().is_some() {
                return true;
            }
        }
        // A re-rooted view unwinds one crumb at a time, and it unwinds BEFORE
        // the panel closes: walking three branches in and hitting esc must
        // retrace them, not drop the reader back into the chat.
        if self.state.tab == PanelTab::Tree && self.root_stack.pop().is_some() {
            self.sel = 0;
            return true;
        }
        // The Miller columns unwind ONE at a time, so escape from an agent
        // lands on its phase rather than on the chat.
        if self.state.tab == PanelTab::Workflows && self.wf_level > 0 {
            self.wf_level -= 1;
            self.wf_scroll = 0;
            self.prompt_open = false;
            if self.wf_level == 0 {
                self.run_detail = None;
            }
            return true;
        }
        false
    }

    fn row_count(&self) -> usize {
        match self.state.tab {
            PanelTab::Tree => self.rows().len(),
            PanelTab::Changes => self.items.len(),
            PanelTab::Workflows => self.runs.len(),
            PanelTab::Mcp => self.mcp.as_ref().map(|s| mcp_names(s).len()).unwrap_or(0),
            PanelTab::Skills => self.filtered_skills().len(),
            PanelTab::Hooks => self.hooks.as_ref().map(|h| h.len()).unwrap_or(0),
            PanelTab::Plugins => self.plugin_rows().len(),
            PanelTab::Model => self.model_entries().len(),
            // Not a list: it has no rows to land a cursor on.
            PanelTab::Context | PanelTab::Theme | PanelTab::Recap => 0,
        }
    }

    /// The agents visible at levels 2–3: the selected phase's, through the `f`
    /// filter. One derivation, used by the cursor AND the renderer.
    fn shown_agents(&self) -> Vec<bough_core::workflow::control::WorkflowAgentView> {
        let Some(detail) = &self.run_detail else {
            return Vec::new();
        };
        let groups = phase_groups(&detail.workflow, &detail.agents);
        let Some(group) = groups.get(self.phase_sel.min(groups.len().saturating_sub(1))) else {
            return Vec::new();
        };
        visible_agents(&group.agents, self.wf_filter())
    }

    /// Cursor movement INSIDE the workflow view, which has three of them —
    /// the run list at level 0, the phase column, the agent column — plus a
    /// scroll at levels 3 and 4. Returns whether it took the keypress.
    fn move_workflow(&mut self, delta: isize) -> bool {
        if self.state.tab != PanelTab::Workflows {
            return false;
        }
        let clamp = |at: isize, len: usize| -> usize {
            at.clamp(0, len.saturating_sub(1) as isize).max(0) as usize
        };
        match self.wf_level {
            0 => return false, // the panel's own `sel`
            1 => {
                let groups = self
                    .run_detail
                    .as_ref()
                    .map(|d| phase_groups(&d.workflow, &d.agents).len())
                    .unwrap_or(0);
                self.phase_sel = clamp(self.phase_sel as isize + delta, groups);
                // A phase change retargets the agent column: leaving the cursor
                // on row 9 of a phase with two agents is a cursor on nothing.
                self.agent_sel = 0;
            }
            2 => {
                let shown = self.shown_agents().len();
                self.agent_sel = clamp(self.agent_sel as isize + delta, shown);
            }
            // The agent detail and the script SCROLL rather than select.
            _ => self.wf_scroll = (self.wf_scroll as isize + delta).max(0) as usize,
        }
        true
    }

    /// `reduce_panel` with the preview attached. The preview travels WITH
    /// every state change because that function is the ONE place that knows
    /// you left the theme tab, and there are five ways to leave it.
    fn reduce_with_theme(&mut self, action: PanelAction) -> PanelState {
        let theme = self
            .theme
            .as_mut()
            .map(|t| t as &mut dyn super::ThemePreview);
        super::reduce_panel(self.state, action, theme)
    }

    fn move_to(&mut self, at: isize) {
        let last = self.row_count().saturating_sub(1) as isize;
        self.sel = at.clamp(0, last.max(0)) as usize;
        // The cursor moving is also the arming being dropped.
        self.pending = None;
        self.model_land = false;
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
        // 2. `esc esc`, ahead of `panel_action_for` for the same reason `e` and
        // `m` are: it is about the landing ROW, which lives here.
        if command == Command::TreeRewind {
            return self.rewind();
        }
        if let Some(action) = panel_action_for(command) {
            let before = self.state;
            match action {
                PanelAction::Move(delta) => {
                    if self.state.tab == PanelTab::Changes && self.diff_focused {
                        self.diff_scroll = (self.diff_scroll as isize + delta).max(0) as usize;
                    } else if self.state.tab == PanelTab::Theme {
                        // Moving here PAINTS, and `reduce_panel` is where that
                        // is written — the preview owns its own cursor
                        // (clamped, never wrapping), so the panel's `sel` is
                        // deliberately not advanced beside it.
                        self.state = self.reduce_with_theme(PanelAction::Move(delta));
                    } else if self.move_workflow(delta) {
                        // Levels 1–4 have their own cursors; level 0 falls
                        // through to the panel's.
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
                        return self.confirm_at(self.sel, true);
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
                            let id = id.clone();
                            self.expanded.insert(id.clone());
                            return self.fetch_on_expand(&id);
                        }
                        Some(ForestRow::Collapsed { origin_id, .. }) => {
                            let origin_id = origin_id.clone();
                            self.drilled.insert(origin_id.clone());
                            return self.fetch_on_expand(&origin_id);
                        }
                        // `→` on a turn unfolds the work it did. The chip said
                        // `▸ 5 tools`; this is what those five were.
                        Some(ForestRow::Message { id, tools, .. }) if *tools > 0 => {
                            self.tools_open.insert(id.clone());
                        }
                        // `→` walks INTO a branch, which is the same move as ⏎
                        // on one — the collapsed row is a door either way.
                        Some(ForestRow::Branch { id, active, .. }) if !*active => {
                            let id = id.clone();
                            self.expanded.insert(id.clone());
                            self.root_stack.push(id.clone());
                            self.sel = 0;
                            return self.fetch_on_expand(&id);
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
                        // An unfolded turn folds its tools back up FIRST — `←`
                        // undoes the `→` that opened them before it undoes the
                        // one that opened the conversation.
                        Some(ForestRow::Message { id, tools_open, .. }) if *tools_open => {
                            let id = id.clone();
                            self.tools_open.remove(&id);
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
                let delta = if command == Command::MovePageUp {
                    -page
                } else {
                    page
                };
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
            // ---- the workflows tab's steering (spec §8) --------------------
            //
            // The verbs act on the run in view — the opened one at levels 1–4,
            // the SELECTED row at level 0 — so a verb that works and is never
            // reachable from the list is a verb nobody has.
            Command::WfPause | Command::WfResume | Command::WfStop | Command::WfRerun => {
                let Some(id) = self.wf_target() else {
                    return Vec::new();
                };
                let action = match command {
                    Command::WfPause => WorkflowAction::Pause,
                    Command::WfResume => WorkflowAction::Resume,
                    Command::WfStop => WorkflowAction::Stop,
                    _ => WorkflowAction::Rerun,
                };
                vec![HostRequest::SteerWorkflow { id, action }]
            }
            Command::WfSave => match self.wf_target() {
                Some(id) => vec![HostRequest::SaveWorkflow(id)],
                None => Vec::new(),
            },
            // `e` is the script LEVEL, and it needs the run's body — the level
            // is refused rather than opened over a header full of nothing.
            Command::WfScript => {
                if self.state.tab == PanelTab::Workflows {
                    match (self.run_detail.is_some(), self.wf_target()) {
                        (true, _) => {
                            self.wf_level = 4;
                            self.wf_scroll = 0;
                        }
                        (false, Some(id)) => return vec![HostRequest::LoadWorkflow(id)],
                        (false, None) => self.message = Some(NO_RUN_SELECTED.to_string()),
                    }
                }
                Vec::new()
            }
            Command::WfFilter => {
                if self.state.tab == PanelTab::Workflows {
                    self.wf_filter = (self.wf_filter + 1) % WF_FILTERS.len();
                    // The filter shortens the list under the cursor.
                    self.agent_sel = 0;
                }
                Vec::new()
            }
            // `o` opens the agent's BACKING SESSION — the drill-in that makes a
            // fan-out's work readable rather than summarised.
            Command::WfOpenAgent => {
                if self.state.tab != PanelTab::Workflows || self.wf_level < 2 {
                    return Vec::new();
                }
                let shown = self.shown_agents();
                match shown
                    .get(self.agent_sel)
                    .and_then(|a| a.agent.session_id.clone())
                {
                    Some(id) => {
                        self.state.open = false;
                        vec![HostRequest::OpenAgentSession(id)]
                    }
                    None => {
                        self.message = Some(NO_AGENT_SESSION.to_string());
                        Vec::new()
                    }
                }
            }
            // ---- the mcp tab's verbs ---------------------------------------
            Command::McpAdd => {
                if self.state.tab == PanelTab::Mcp {
                    // An empty buffer, opened. The name is derived from the URL
                    // at registration time, never typed.
                    self.mcp_entry = Some(String::new());
                    self.mcp_pending_delete = None;
                }
                Vec::new()
            }
            Command::McpConnect => match self.mcp_selected() {
                Some(name) => vec![HostRequest::ConnectMcpServer(name)],
                None => Vec::new(),
            },
            Command::McpRestart => match self.mcp_selected() {
                Some(name) => vec![HostRequest::RestartMcpServer(name)],
                None => Vec::new(),
            },
            Command::McpAuth => match self.mcp_selected() {
                Some(name) => vec![HostRequest::BeginMcpAuth(name)],
                None => Vec::new(),
            },
            Command::McpForget => match self.mcp_selected() {
                Some(name) => vec![HostRequest::ClearMcpAuth(name)],
                None => Vec::new(),
            },
            // Two keypresses, like every destructive verb here: `d` arms and
            // names what it will drop, `d` again performs it.
            Command::McpRemove => {
                let Some(name) = self.mcp_selected() else {
                    return Vec::new();
                };
                match self.mcp_pending_delete.take() {
                    Some(armed) if armed == name => {
                        self.message = None;
                        vec![HostRequest::DeleteMcpServer(name)]
                    }
                    _ => {
                        self.message = Some(format!(
                            "d again deletes the registration for {name} — credentials are kept; F forgets those"
                        ));
                        self.mcp_pending_delete = Some(name);
                        Vec::new()
                    }
                }
            }
            // ---- the model tab ---------------------------------------------
            //
            // ⇥ between the two boxes. Two boxes and not one, because picking a
            // frontier model and a cheap one is a single decision about a pair.
            Command::PanelFilterTier => {
                if self.state.tab == PanelTab::Model {
                    self.model_focus = Some(match self.model_focus {
                        Some(Tier::Frontier) => Tier::Cheap,
                        _ => Tier::Frontier,
                    });
                }
                Vec::new()
            }
            // 3./4. The surgery verbs. Nothing is destroyed by either — the
            // source keeps every turn — so neither needs the arm-and-confirm
            // that `changes.revert` does.
            Command::TreeExtract => {
                if self.state.tab != PanelTab::Tree {
                    return Vec::new();
                }
                let Some((id, session_id)) = self.turn_row() else {
                    self.message = Some(EXTRACT_NEEDS_A_TURN.to_string());
                    return Vec::new();
                };
                let Some(picks) = self.picks_from(&session_id, &id) else {
                    self.message = Some(TURN_IS_GONE.to_string());
                    return Vec::new();
                };
                self.state.open = false;
                vec![HostRequest::Extract { session_id, picks }]
            }
            Command::TreeMoveInto => {
                if self.state.tab != PanelTab::Tree {
                    return Vec::new();
                }
                let Some((id, session_id)) = self.turn_row() else {
                    self.message = Some(MOVE_NEEDS_A_TURN.to_string());
                    return Vec::new();
                };
                let Some(target_id) = self.current_id.clone() else {
                    self.message = Some(MOVE_NEEDS_A_TARGET.to_string());
                    return Vec::new();
                };
                if session_id == target_id {
                    // Said locally rather than as a 400: this is the likely
                    // slip, and "a session cannot append its own turns to its
                    // own tail" reads as a fault when it is really the wrong
                    // row. The server's three unsound-target refusals (itself,
                    // a session mid-turn, an ancestor of the source) still
                    // arrive as sentences.
                    self.message = Some(MOVE_IS_THE_SAME_CONVERSATION.to_string());
                    return Vec::new();
                }
                let Some(picks) = self.picks_from(&session_id, &id) else {
                    self.message = Some(TURN_IS_GONE.to_string());
                    return Vec::new();
                };
                self.state.open = false;
                vec![HostRequest::MoveInto {
                    target_id,
                    source_id: session_id,
                    picks,
                }]
            }
            // 12. The `/` buffer (row 3.21). In the tree it is a full-text
            // search of every message; the debounced fetch is the app's, and
            // the panel only owns the text.
            Command::PanelFilter => {
                self.filtering = true;
                // The model tab has TWO boxes; `/` lands on the frontier one and
                // ⇥ crosses. Every other filtering tab has one.
                if self.state.tab == PanelTab::Model && self.model_focus.is_none() {
                    self.model_focus = Some(Tier::Frontier);
                }
                Vec::new()
            }
            Command::PanelFilterBack => {
                match self.model_box() {
                    Some(buffer) => {
                        buffer.pop();
                    }
                    None => {
                        self.filter.pop();
                    }
                }
                self.move_to(0);
                Vec::new()
            }
            // esc CLEARS: a narrowed list nobody can see the query for is a
            // list that looks broken.
            Command::PanelFilterExit => {
                self.filtering = false;
                match self.model_box() {
                    Some(buffer) => buffer.clear(),
                    None => self.filter.clear(),
                }
                self.model_focus = None;
                self.matched_sessions.clear();
                self.matched_messages.clear();
                self.move_to(0);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The run a steering verb acts on: the OPENED one at levels 1–4, the
    /// selected row at level 0. `None` outside the tab, or with an empty list.
    fn wf_target(&self) -> Option<String> {
        if self.state.tab != PanelTab::Workflows {
            return None;
        }
        if let Some(detail) = self.run_detail.as_ref().filter(|_| self.wf_level > 0) {
            return Some(detail.workflow.id.clone());
        }
        self.runs.get(self.sel).map(|r| r.id.clone())
    }

    /// The registered server under the cursor. `None` outside the tab.
    fn mcp_selected(&self) -> Option<String> {
        if self.state.tab != PanelTab::Mcp {
            return None;
        }
        mcp_names(self.mcp.as_ref()?).get(self.sel).cloned()
    }

    /// The tier box the keyboard is typing into, if any. Returned as a mutable
    /// borrow so `/`'s three commands each edit exactly one buffer.
    fn model_box(&mut self) -> Option<&mut String> {
        if self.state.tab != PanelTab::Model {
            return None;
        }
        match self.model_focus? {
            Tier::Cheap => Some(&mut self.model_filters.cheap),
            _ => Some(&mut self.model_filters.frontier),
        }
    }

    /// One printable character into the `/` buffer. Unbound keys reach here
    /// only while [`Self::filtering`], which is what the keymap's
    /// `panelFiltering` guard is for.
    pub fn type_filter(&mut self, c: char) {
        if !self.filtering {
            // The MCP URL buffer is its OWN modal buffer and is not the `/`
            // filter: it takes text while it is open, and the filter guard
            // never applies to this tab.
            if self.state.tab == PanelTab::Mcp {
                if let Some(entry) = self.mcp_entry.as_mut() {
                    entry.push(c);
                }
            }
            return;
        }
        match self.model_box() {
            Some(buffer) => buffer.push(c),
            None => self.filter.push(c),
        }
        self.move_to(0);
    }

    /// ⌫ inside the MCP URL buffer, which the `/` filter's own backspace does
    /// not reach (the two buffers are never open at once, and the guard that
    /// routes the filter's keys is off in this tab).
    pub fn mcp_entry_back(&mut self) -> bool {
        match self.mcp_entry.as_mut() {
            Some(entry) => {
                entry.pop();
                true
            }
            None => false,
        }
    }

    /// What `GET /search` answered for `q`, ignored when the buffer has moved
    /// on — a reply for a query the user has already typed past must not mark
    /// rows against words that are no longer on screen.
    pub fn set_search_hits(&mut self, q: &str, sessions: Vec<String>, messages: Vec<String>) {
        if q != self.filter.trim() {
            return;
        }
        // EXPAND each hit: a conversation's turns only render when it is open,
        // so marking the matching turn and leaving the row collapsed would be
        // marking something nobody can see.
        for id in &sessions {
            self.expanded.insert(id.clone());
        }
        self.matched_sessions = sessions;
        self.matched_messages = messages;
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
            // The run LIST only: at a detail level a digit would open a run
            // other than the one on screen.
            PanelTab::Workflows if self.wf_level == 0 => {
                let height = wf_runs_height(body_rows);
                let (_, from) = super::workflows::windowed(&self.runs, self.sel, height);
                let at = from + digit - 1;
                (digit <= height && at < self.runs.len()).then_some(at)
            }
            PanelTab::Mcp => {
                let names = self.mcp.as_ref().map(mcp_names).unwrap_or_default();
                let chrome =
                    usize::from(self.message.is_some()) + usize::from(self.mcp_entry.is_some());
                let (start, _, height, _) =
                    super::mcp::mcp_window(names.len(), self.sel, body_rows, chrome);
                let at = start + digit - 1;
                (digit <= height && at < names.len()).then_some(at)
            }
            PanelTab::Skills => {
                let skills = self.filtered_skills();
                let chrome = usize::from(self.filtering || !self.skills_filter().is_empty())
                    + usize::from(!self.skill_sources.is_empty());
                let (start, height, _) =
                    super::skills::skills_window(skills.len(), self.sel, body_rows, chrome);
                let at = start + digit - 1;
                (digit <= height && at < skills.len()).then_some(at)
            }
            // The digits address ENTRIES, not the headers and hints between
            // them — resolved against the same window the picker paints.
            PanelTab::Model => {
                let entries = self.model_entries();
                let display = display_rows(
                    &entries,
                    self.model_cfg.cheap_model.is_none(),
                    &self.model_filters,
                    self.model_focus,
                );
                let chrome = usize::from(self.message.is_some());
                let (start, end, _, _) = model_window(&display, self.sel, body_rows, chrome);
                visible_entries(&display, start, end)
                    .get(digit - 1)
                    .copied()
            }
            // changes/theme: none — a digit that jumped-and-affirmed would
            // revert a file you never saw.
            _ => None,
        }
    }

    /// What ⏎ affirms, per tab.
    pub fn confirm(&mut self, at: usize) -> Vec<HostRequest> {
        self.confirm_at(at, false)
    }

    /// ⏎ (`summarize = false`) and the tree's `s` (`summarize = true`), which
    /// differ only in whether the branch carries a summary of the path it left
    /// behind. One body, because `s` IS the ordinary commit on the tree and
    /// must not become a second one that drifts from it.
    pub fn confirm_at(&mut self, at: usize, summarize: bool) -> Vec<HostRequest> {
        match self.state.tab {
            // Nothing to confirm: the tab is a report, not a list.
            PanelTab::Context | PanelTab::Recap => vec![],
            // ⏎ and space do the same thing here: the row has exactly one
            // verb, and a list where enter does nothing teaches the user that
            // the list is inert.
            // The plugin's own row and its items are the same kind of switch,
            // so ⏎ means one thing here. Which STORE holds the switch is the
            // server's business (`plugins::set_enabled`), not this cursor's.
            PanelTab::Plugins => self
                .plugin_rows()
                .get(at)
                .map(|row| {
                    vec![HostRequest::TogglePlugin {
                        id: row.id.clone(),
                        // The row's OWN switch, never the effective one: ⏎ on
                        // an item under a disabled plugin must not read as
                        // "it is off, so turn it on" and silently flip a
                        // switch that was already on.
                        enabled: !row.enabled,
                    }]
                })
                .unwrap_or_default(),
            PanelTab::Hooks => self
                .hooks
                .as_ref()
                .and_then(|h| h.get(at))
                .map(|h| {
                    vec![HostRequest::ToggleHook {
                        // The ID, never the file name: two sources can ship
                        // the same name and only one of them is under the
                        // cursor.
                        name: h.id.clone(),
                        enabled: !h.enabled,
                    }]
                })
                .unwrap_or_default(),
            PanelTab::Tree => {
                let rows = self.rows();
                let Some(row) = rows.get(at) else {
                    return Vec::new();
                };
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
                    // The view moves; nothing about the CONVERSATION does. It
                    // is the cheap way to read a branch you did not take, and
                    // `esc` walks straight back out of it.
                    Selection::ReRoot(id) => {
                        self.expanded.insert(id.clone());
                        self.root_stack.push(id.clone());
                        self.sel = 0;
                        self.fetch_on_expand(&id)
                    }
                    Selection::Fork {
                        session_id,
                        at_message_id,
                        exclusive,
                        editor_text,
                    } => {
                        self.state.open = false;
                        vec![HostRequest::Fork {
                            session_id,
                            at_message_id,
                            exclusive,
                            summarize_abandoned: summarize,
                            editor_text,
                        }]
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
                let Some(state) = self.theme.as_ref().and_then(|p| p.baseline().cloned()) else {
                    return Vec::new();
                };
                vec![HostRequest::SaveTheme(crate::theme::persist_request(
                    &state,
                ))]
            }
            // ⏎ DESCENDS one Miller column: runs → phases → agents → one
            // agent, and on an open agent it toggles the prompt fold.
            PanelTab::Workflows => match self.wf_level {
                0 => match self.runs.get(at) {
                    Some(run) => {
                        self.wf_level = 1;
                        self.phase_sel = 0;
                        self.agent_sel = 0;
                        self.wf_scroll = 0;
                        vec![HostRequest::LoadWorkflow(run.id.clone())]
                    }
                    None => Vec::new(),
                },
                1 => {
                    self.wf_level = 2;
                    self.agent_sel = 0;
                    Vec::new()
                }
                2 => {
                    if !self.shown_agents().is_empty() {
                        self.wf_level = 3;
                        self.wf_scroll = 0;
                        self.prompt_open = false;
                    }
                    Vec::new()
                }
                // The prompt is COLLAPSED by default — it is the one thing you
                // already know, you wrote the workflow.
                3 => {
                    self.prompt_open = !self.prompt_open;
                    Vec::new()
                }
                _ => Vec::new(),
            },
            // ⏎ GRANTS, or registers what the URL buffer holds. The buffer
            // takes ⏎ before the list does — see the tab's own legend.
            PanelTab::Mcp => {
                if let Some(url) = self.mcp_entry.take() {
                    let url = url.trim().to_string();
                    if url.is_empty() {
                        return Vec::new();
                    }
                    let taken = self.mcp.as_ref().map(mcp_names).unwrap_or_default();
                    let name = name_from_url(&url, &taken);
                    if name.is_empty() {
                        self.message = Some(NOT_A_SERVER_URL.to_string());
                        return Vec::new();
                    }
                    return vec![HostRequest::AddMcpServer { name, url }];
                }
                let names = self.mcp.as_ref().map(mcp_names).unwrap_or_default();
                let Some(name) = names.get(at) else {
                    return Vec::new();
                };
                let granted = self
                    .mcp
                    .as_ref()
                    .map(|s| s.active.contains(name))
                    .unwrap_or(false);
                vec![HostRequest::SetMcpEnabled {
                    name: name.clone(),
                    enabled: !granted,
                }]
            }
            // A skill is loaded by NAMING it in the composer, which is what the
            // legend says; ⏎ here says so rather than pretending to run one.
            PanelTab::Skills => {
                if let Some(skill) = self.filtered_skills().get(at) {
                    self.message = Some(format!("type /{} in the composer to load it", skill.name));
                }
                Vec::new()
            }
            // ⏎ CHOOSES. `choose_entry` is spec §12 in code: a frontier pick
            // pins this session AND moves the default for new sessions, a cheap
            // pick moves the install's one background model and nothing else.
            // The write is one request carrying the whole config, so a caller
            // cannot perform half of it.
            //
            // The config is only mutated once a request is actually going out.
            // It used to be assigned before the cheap tier's "nothing was
            // saved" refusal below it, so the ● relocated to the row the user
            // picked while the same screen said the write had not happened —
            // the dead control the refusal existed to prevent.
            PanelTab::Model => {
                let entries = self.model_entries();
                let Some(entry) = entries.get(at) else {
                    return Vec::new();
                };
                self.model_cfg = super::model::choose_entry(&self.model_cfg, entry);
                vec![HostRequest::SaveModel(self.model_cfg.clone())]
            }
        }
    }
}

/// A registry name derived from the server's URL — the user types the URL and
/// nothing else, so the name is this function's job.
///
/// The `mcp.`/`www.`/`api.` prefixes carry no information (every third MCP
/// endpoint is `mcp.something`), and the TLD carries less. A `co.uk`-shaped
/// suffix loses BOTH parts, or "taking the part before the TLD" would name the
/// server `co`. A collision gets a `-2` suffix rather than overwriting:
/// silently replacing a registration that may already hold credentials is the
/// one outcome worse than asking.
pub fn name_from_url(raw: &str, taken: &[String]) -> String {
    let Some(host) = url_host(raw) else {
        return String::new();
    };
    let parts: Vec<&str> = host
        .split('.')
        .filter(|p| !p.is_empty() && *p != "mcp" && *p != "www" && *p != "api")
        .collect();
    let kept: &[&str] = if parts.len() > 2 && parts[parts.len() - 2].len() <= 3 {
        &parts[..parts.len() - 2]
    } else if parts.is_empty() {
        &[]
    } else {
        &parts[..parts.len() - 1]
    };
    let base = kept
        .last()
        .copied()
        .or_else(|| parts.first().copied())
        .unwrap_or(&host);
    let slug: String = base
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        return String::new();
    }
    if !taken.contains(&slug) {
        return slug;
    }
    for i in 2..100 {
        let candidate = format!("{slug}-{i}");
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    slug
}

/// The hostname of an absolute URL, or `None` — a scheme is required, so
/// `"linear"` is not a URL and names nothing.
fn url_host(raw: &str) -> Option<String> {
    let (scheme, rest) = raw.split_once("://")?;
    if scheme.is_empty() || rest.is_empty() {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Strip userinfo, then the port.
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = authority.split(':').next().unwrap_or("");
    (!host.is_empty()).then(|| host.to_lowercase())
}

/// The refusals, verbatim. Every one names the gesture that WOULD work.
pub const NO_RUN_SELECTED: &str = "no run selected — ⏎ opens one first";
pub const NO_AGENT_SESSION: &str =
    "no session — this call was replayed from the journal, so there is nothing to open";
pub const NOT_A_SERVER_URL: &str = "that is not a URL bough can name a server from";
pub const EXTRACT_NEEDS_A_TURN: &str = "e splits a conversation at a TURN — move onto one first";
pub const MOVE_NEEDS_A_TURN: &str = "m brings a TURN here — move onto one first";
pub const MOVE_NEEDS_A_TARGET: &str = "no conversation is open to bring these turns into";
pub const MOVE_IS_THE_SAME_CONVERSATION: &str = "those turns are already in this conversation";
pub const TURN_IS_GONE: &str = "that turn is no longer in the thread";
pub const REWIND_NEEDS_A_CONVERSATION: &str =
    "no conversation is open — there is no turn to go back to";
pub const NOTHING_TO_REVERT: &str = "nothing to revert here";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forest::fixtures::{msg, session_row, with_origin};
    use bough_core::schema::parts::{Role, SessionKind};
    use serde_json::json;

    fn host() -> PanelHost {
        let mut h = PanelHost {
            sessions: vec![
                session_row("a", SessionKind::Root, 1),
                session_row("b", SessionKind::Root, 2),
            ],
            ..Default::default()
        };
        h.threads = HashMap::from([(
            "a".to_string(),
            vec![
                msg("m1", Role::User, "go"),
                msg("m2", Role::Supervisor, "done"),
            ],
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
        assert!(
            h.diff_focused,
            "the focus must survive cancelling the revert"
        );
        assert!(h.open());
        h.handle(Command::PanelClose, None, 10);
        assert!(!h.diff_focused);
        assert!(h.open());
        h.handle(Command::PanelClose, None, 10);
        assert!(!h.open());
    }

    /// 2b: reading a branch you did not take costs no indentation and no
    /// commitment — the view re-roots, and `esc` retraces the way in.
    #[test]
    fn entering_a_collapsed_branch_re_roots_the_view_and_esc_walks_back_out() {
        let mut h = host();
        h.state.tab = PanelTab::Tree;
        h.state.open = true;
        let root = session_row("root", SessionKind::Root, 1);
        let mut mine = with_origin(session_row("mine", SessionKind::Fork, 3), "root");
        mine.session.origin_message_id = Some("m1".into());
        let mut other = with_origin(session_row("other", SessionKind::Fork, 2), "root");
        other.session.origin_message_id = Some("m1".into());
        other.session.title = "try a cursor-based approach".into();
        h.current_id = Some("mine".into());
        h.threads.insert(
            "root".into(),
            vec![msg("m1", Role::User, "refactor the offsets")],
        );
        h.threads
            .insert("other".into(), vec![msg("mo", Role::User, "cursor")]);
        h.expanded.insert("root".into());
        h.set_sessions(vec![root, mine, other]);

        // `mine` carries the trunk, so `other` is the collapsed sibling.
        let at = h
            .rows()
            .iter()
            .position(|r| matches!(r, ForestRow::Branch { id, active: false, .. } if id == "other"))
            .expect("the sibling must be one collapsed row");
        h.confirm_at(at, false);
        assert_eq!(h.root_stack, vec!["other".to_string()]);
        assert_eq!(h.crumbs(), vec!["try a cursor-based approach".to_string()]);
        // The re-rooted view is that branch's turns, and nothing above them.
        assert_eq!(
            h.rows()
                .iter()
                .map(|r| r.id().to_string())
                .collect::<Vec<_>>(),
            vec!["mo".to_string()]
        );
        // NOTHING MOVED but the view: re-rooting is not switching.
        assert_eq!(h.current_id.as_deref(), Some("mine"));
        assert!(h.open(), "and the panel is still up");

        h.handle(Command::PanelClose, None, 10);
        assert!(h.root_stack.is_empty(), "esc retraces the crumb");
        assert!(h.open(), "…before it closes anything");
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
        assert_eq!(
            h.sel, 0,
            "the file cursor must not move under a focused diff"
        );
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
        let collapsed = h
            .rows()
            .iter()
            .position(|r| matches!(r, ForestRow::Collapsed { .. }))
            .unwrap();
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
        assert_eq!(
            h.handle(Command::PanelPick, Some(2), 10),
            vec![HostRequest::Open("a".into())]
        );
        // A digit past the window does nothing — no clamp, no nearest row.
        let mut h = host();
        h.handle(Command::PanelToggle, None, 10);
        assert!(h.handle(Command::PanelPick, Some(9), 10).is_empty());
        assert!(h.open());
    }

    /// The tree re-reads `GET /sessions` on the rail's beat. Landing on every
    /// answer re-parked the cursor about once a second — ↓ moved a row and the
    /// next poll pulled it back to "you are here" — which put every verb below
    /// the current conversation (⏎ to fork, `e`, `m`) out of reach by hand.
    #[test]
    fn a_refreshed_listing_does_not_move_a_cursor_the_user_placed() {
        let mut h = host();
        h.current_id = Some("a".into());
        h.handle(Command::PanelToggle, None, 10);
        // The ARRIVAL's own listing lands on you-are-here.
        h.set_sessions(vec![
            session_row("a", SessionKind::Root, 1),
            session_row("b", SessionKind::Root, 2),
        ]);
        assert_eq!(h.rows()[h.sel].id(), "a");
        h.handle(Command::MoveDown, None, 10);
        let moved = h.sel;
        assert_ne!(moved, 0, "the cursor moved");
        // …and the next poll leaves it exactly where the user put it.
        h.set_sessions(vec![
            session_row("a", SessionKind::Root, 1),
            session_row("b", SessionKind::Root, 2),
        ]);
        assert_eq!(h.sel, moved);
    }

    /// Move the cursor onto `a`'s first turn, which every surgery test needs.
    fn on_a_turn(h: &mut PanelHost) {
        h.expanded.insert("a".into());
        let turn = h.rows().iter().position(|r| r.id() == "m1").unwrap();
        h.move_to(turn as isize);
    }

    #[test]
    fn the_surgery_verbs_refuse_out_loud_and_name_the_gesture_that_would_work() {
        let mut h = host();
        h.handle(Command::PanelToggle, None, 10);
        // On a conversation row: `e` says what it needs.
        assert!(h.handle(Command::TreeExtract, None, 10).is_empty());
        assert_eq!(h.message.as_deref(), Some(EXTRACT_NEEDS_A_TURN));
        assert!(h.handle(Command::TreeMoveInto, None, 10).is_empty());
        assert_eq!(h.message.as_deref(), Some(MOVE_NEEDS_A_TURN));
    }

    /// `e` — the turn under the cursor and every LATER turn of ITS conversation
    /// become a fresh root. The source keeps its own, so nothing is armed.
    #[test]
    fn extract_picks_that_turn_and_every_later_turn_of_its_own_thread() {
        let mut h = host();
        h.handle(Command::PanelToggle, None, 10);
        on_a_turn(&mut h);
        assert_eq!(
            h.handle(Command::TreeExtract, None, 10),
            vec![HostRequest::Extract {
                session_id: "a".into(),
                picks: vec![
                    PartPick {
                        message_id: "m1".into(),
                        parts: None
                    },
                    PartPick {
                        message_id: "m2".into(),
                        parts: None
                    },
                ],
            }]
        );
        assert!(!h.open(), "the panel closes onto the conversation it made");
    }

    /// `m` — extract's mirror. Three local refusals, then the copy.
    #[test]
    fn move_into_refuses_locally_before_it_asks_the_server() {
        let mut h = host();
        h.handle(Command::PanelToggle, None, 10);
        on_a_turn(&mut h);
        // No conversation open to receive them.
        assert!(h.handle(Command::TreeMoveInto, None, 10).is_empty());
        assert_eq!(h.message.as_deref(), Some(MOVE_NEEDS_A_TARGET));
        // The row's OWN conversation is the open one.
        h.current_id = Some("a".into());
        assert!(h.handle(Command::TreeMoveInto, None, 10).is_empty());
        assert_eq!(h.message.as_deref(), Some(MOVE_IS_THE_SAME_CONVERSATION));
        // A real target: the same picks, landing on `b`'s tail.
        h.current_id = Some("b".into());
        assert_eq!(
            h.handle(Command::TreeMoveInto, None, 10),
            vec![HostRequest::MoveInto {
                target_id: "b".into(),
                source_id: "a".into(),
                picks: vec![
                    PartPick {
                        message_id: "m1".into(),
                        parts: None
                    },
                    PartPick {
                        message_id: "m2".into(),
                        parts: None
                    },
                ],
            }]
        );
        assert!(!h.open());
    }

    /// ⏎ on a turn FORKS — and a USER turn cuts BEFORE itself, handing its text
    /// to the composer so the re-send IS the new branch.
    #[test]
    fn confirm_on_a_turn_forks_the_rows_own_conversation() {
        let mut h = host();
        h.handle(Command::PanelToggle, None, 10);
        on_a_turn(&mut h);
        assert_eq!(
            h.confirm(h.sel),
            vec![HostRequest::Fork {
                session_id: "a".into(),
                at_message_id: "m1".into(),
                exclusive: true,
                summarize_abandoned: false,
                editor_text: Some("go".into()),
            }]
        );
        assert!(!h.open());
    }

    #[test]
    fn summarize_fork_acts_on_the_tree_and_nowhere_else() {
        let mut h = host();
        h.handle(Command::Tab(PanelTab::Changes), None, 10);
        assert!(h
            .handle(Command::PanelConfirmSummarize, None, 10)
            .is_empty());
        assert_eq!(
            h.message, None,
            "`s` outside the tree must not affirm anything"
        );
        h.handle(Command::Tab(PanelTab::Tree), None, 10);
        on_a_turn(&mut h);
        // The SAME fork, with the abandoned path carried onto the branch.
        assert_eq!(
            h.handle(Command::PanelConfirmSummarize, None, 10),
            vec![HostRequest::Fork {
                session_id: "a".into(),
                at_message_id: "m1".into(),
                exclusive: true,
                summarize_abandoned: true,
                editor_text: Some("go".into()),
            }]
        );
    }

    /// `esc esc` — the tree, opened ON the turn you would go back to.
    #[test]
    fn rewind_opens_the_tree_on_the_open_conversations_last_user_turn() {
        let mut h = host();
        h.current_id = Some("a".into());
        let requests = h.handle(Command::TreeRewind, None, 10);
        assert!(h.open() && h.tab() == PanelTab::Tree);
        assert!(requests.contains(&HostRequest::LoadSessions));
        assert!(
            h.expanded.contains("a"),
            "the conversation must be expanded, or its turns are not rows at all"
        );
        assert_eq!(h.rows()[h.sel].id(), "m1", "the last USER turn");
        // With no conversation open there is nothing to go back to, and the
        // tree still opens — at the top, where the switcher lives.
        let mut h = host();
        h.handle(Command::TreeRewind, None, 10);
        assert_eq!(h.message.as_deref(), Some(REWIND_NEEDS_A_CONVERSATION));
        assert!(h.open());
    }

    /// The rail's ⏎ on a run: the workflows tab, drilled in on THAT run.
    #[test]
    fn open_run_lands_the_workflows_tab_on_one_run() {
        let mut h = host();
        assert_eq!(
            h.open_run("w1"),
            vec![
                HostRequest::LoadWorkflows,
                HostRequest::LoadWorkflow("w1".into()),
            ]
        );
        assert!(h.open() && h.tab() == PanelTab::Workflows);
        assert_eq!(h.wf_level, 1);
    }

    #[test]
    fn the_reveal_path_seeds_the_expansion_so_the_open_conversation_is_on_screen() {
        let mut h = PanelHost {
            current_id: Some("hand".into()),
            ..Default::default()
        };
        let root = session_row("root", SessionKind::Root, 1);
        let hand = with_origin(session_row("hand", SessionKind::Root, 2), "root");
        h.handle(Command::PanelToggle, None, 10);
        h.set_sessions(vec![root, hand]);
        assert!(
            h.expanded.contains("root"),
            "the origin must be opened to reach the current row"
        );
        assert_eq!(h.rows()[h.sel].id(), "hand");
    }

    // ---- the theme tab (Theme.tsx's half of PanelHost.tsx) ------------------

    /// A host already on the theme tab, with a preview that paints NOWHERE —
    /// `ThemePreview::new` would drive the process-global palette, and a unit
    /// test that repaints the terminal is a test that fails under `--jobs`.
    fn themed() -> PanelHost {
        let mut h = PanelHost::default();
        h.handle(Command::Tab(PanelTab::Theme), None, 10);
        h.theme = Some(crate::theme::ThemePreview::with_apply(
            None,
            Box::new(|_| {}),
        ));
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
            assert_eq!(
                preview.index(),
                0,
                "{leave:?} must restore the baseline row"
            );
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
        assert_eq!(
            h.theme.as_ref().unwrap().index(),
            1,
            "a kept palette survives leaving"
        );
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
        assert_eq!(
            h.theme.as_ref().unwrap().index(),
            0,
            "1-9 is not a theme gesture"
        );
    }

    /// DEFECT 2. `panel.threads` was filled exclusively from the OPEN session,
    /// and `→` on any other row just inserted the id into `expanded` — no fetch.
    /// So every other conversation expanded to zero turns and ⏎-fork, `e` and
    /// `m` were unreachable there.
    #[test]
    fn expanding_another_conversation_fetches_its_turns() {
        let mut h = host();
        h.state.open = true;
        h.state.tab = PanelTab::Tree;
        // `b` has no thread in the fixture — it is the unopened conversation.
        h.sel = h
            .rows()
            .iter()
            .position(|r| matches!(r, ForestRow::Session { id, .. } if id == "b"))
            .expect("b is on screen");

        let requests = h.handle(Command::MoveIn, None, 20);
        assert!(h.expanded.contains("b"), "the caret flipped ▸→▾");
        assert!(
            requests.contains(&HostRequest::LoadThread("b".into())),
            "…and something must go and get the turns: {requests:?}"
        );
        assert!(
            requests.contains(&HostRequest::LoadChildSessions("b".into())),
            "…including whatever collapsed under it: {requests:?}"
        );

        // The answers land, and the turns are now rows under `b`.
        h.threads.insert(
            "b".into(),
            vec![
                msg("b1", Role::User, "ask"),
                msg("b2", Role::Supervisor, "answer"),
            ],
        );
        h.set_children("b".into(), Vec::new());
        let under_b = h
            .rows()
            .iter()
            .filter(|r| matches!(r, ForestRow::Message { session_id, .. } if session_id == "b"))
            .count();
        assert_eq!(
            under_b, 2,
            "a conversation that expands to nothing has no verbs"
        );
    }

    /// …and it asks ONCE. The tree re-renders every frame and the rail re-polls
    /// every second; a fetch per expanded row per beat is the N+1 this must not
    /// become.
    #[test]
    fn a_second_expand_of_the_same_row_asks_for_nothing() {
        let mut h = host();
        h.state.open = true;
        h.state.tab = PanelTab::Tree;
        h.sel = h
            .rows()
            .iter()
            .position(|r| matches!(r, ForestRow::Session { id, .. } if id == "b"))
            .expect("b is on screen");
        assert!(!h.handle(Command::MoveIn, None, 20).is_empty());
        // The answers arrive — an EMPTY drill-in is still an answer.
        h.threads.insert("b".into(), Vec::new());
        h.set_children("b".into(), Vec::new());
        assert!(
            h.handle(Command::MoveIn, None, 20).is_empty(),
            "both facts are known; asking again would be a poll"
        );
    }

    /// The open conversation is already mirrored, so expanding it fetches
    /// nothing — the row the user is most likely to press `→` on.
    #[test]
    fn expanding_the_open_conversation_asks_only_for_its_drill_in() {
        let mut h = host();
        h.state.open = true;
        h.state.tab = PanelTab::Tree;
        h.current_id = Some("a".into());
        h.sel = h
            .rows()
            .iter()
            .position(|r| matches!(r, ForestRow::Session { id, .. } if id == "a"))
            .expect("a is on screen");
        let requests = h.handle(Command::MoveIn, None, 20);
        assert!(!requests.contains(&HostRequest::LoadThread("a".into())));
        assert_eq!(requests, vec![HostRequest::LoadChildSessions("a".into())]);
    }

    /// The drill-in rows reach the forest — a subagent is a NODE in the tree,
    /// not only a rail row.
    #[test]
    fn a_drilled_in_subagent_becomes_a_tree_row() {
        let mut h = host();
        h.expanded.insert("a".into());
        h.set_children(
            "a".into(),
            vec![with_origin(
                session_row("sub-1", SessionKind::Subagent, 3),
                "a",
            )],
        );
        h.drilled.insert("a".into());
        assert!(
            h.rows()
                .iter()
                .any(|r| matches!(r, ForestRow::Session { id, .. } if id == "sub-1")),
            "the tree showed zero subagent nodes: {:?}",
            h.rows()
        );
    }
}

// ---------------------------------------------------------------------------
// Tests — row 3.20: the four remaining tabs, through the controller
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tab_tests {
    use super::super::mcp::fixtures as mcp_fx;
    use super::super::model::fixtures as model_fx;
    use super::super::workflows::fixtures as wf_fx;
    use super::*;
    use crate::api::{WorkflowAgentCounts, WorkflowSummary};

    fn open_on(tab: PanelTab) -> PanelHost {
        let mut h = PanelHost::default();
        h.handle(Command::PanelToggle, None, 20);
        h.handle(Command::Tab(tab), None, 20);
        h
    }

    fn run(id: &str, status: &str) -> WorkflowSummary {
        WorkflowSummary {
            id: id.into(),
            name: id.into(),
            description: "a run".into(),
            status: status.into(),
            current_phase: None,
            agents: WorkflowAgentCounts::default(),
            created_at: 0,
            finished_at: None,
        }
    }

    // ---- the workflows tab -------------------------------------------------

    /// A drilled level must not outlive its tab: a header for `run-2` over
    /// another conversation's list is the shape of "the panel remembered a
    /// state nobody can see".
    #[test]
    fn leaving_and_returning_lands_on_the_run_list_not_on_the_last_drill() {
        let mut h = open_on(PanelTab::Workflows);
        h.set_workflows(vec![run("run-2", "running")]);
        h.handle(Command::PanelConfirm, None, 20);
        h.set_workflow_detail(Some(wf_fx::detail()));
        h.handle(Command::PanelConfirm, None, 20);
        assert_eq!(h.wf_level, 2);
        h.handle(Command::Tab(PanelTab::Tree), None, 20);
        assert_eq!(
            h.handle(Command::Tab(PanelTab::Workflows), None, 20),
            vec![HostRequest::LoadWorkflows]
        );
        assert_eq!(h.wf_level, 0);
        assert!(h.run_detail.is_none());
    }

    #[test]
    fn arrival_asks_for_the_run_list() {
        let mut h = PanelHost::default();
        h.handle(Command::PanelToggle, None, 20);
        let requests = h.handle(Command::Tab(PanelTab::Workflows), None, 20);
        assert_eq!(requests, vec![HostRequest::LoadWorkflows]);
    }

    #[test]
    fn enter_descends_the_miller_columns_and_esc_climbs_back_one_at_a_time() {
        let mut h = open_on(PanelTab::Workflows);
        h.set_workflows(vec![run("run-2", "running")]);
        // ⏎ on a run opens it — and asks for the body that carries the replay
        // accounting.
        let requests = h.handle(Command::PanelConfirm, None, 20);
        assert_eq!(requests, vec![HostRequest::LoadWorkflow("run-2".into())]);
        assert_eq!(h.wf_level, 1);
        h.set_workflow_detail(Some(wf_fx::detail()));
        h.handle(Command::PanelConfirm, None, 20);
        assert_eq!(h.wf_level, 2, "phases → that phase's agents");
        h.handle(Command::PanelConfirm, None, 20);
        assert_eq!(h.wf_level, 3, "an agent → its detail");
        // ⏎ at the agent level folds the prompt rather than descending.
        h.handle(Command::PanelConfirm, None, 20);
        assert!(h.prompt_open);
        // esc unwinds ONE level per press, never straight to the chat.
        for expected in [2u8, 1, 0] {
            h.handle(Command::PanelClose, None, 20);
            assert_eq!(h.wf_level, expected);
        }
        assert!(h.open(), "the panel is still open — the levels went first");
    }

    #[test]
    fn the_steering_verbs_act_on_the_run_in_view() {
        let mut h = open_on(PanelTab::Workflows);
        h.set_workflows(vec![run("run-a", "running"), run("run-b", "running")]);
        // At level 0 they act on the SELECTED row: a verb that works only after
        // opening a run is a verb the list advertises and does not have.
        h.handle(Command::MoveDown, None, 20);
        assert_eq!(
            h.handle(Command::WfPause, None, 20),
            vec![HostRequest::SteerWorkflow {
                id: "run-b".into(),
                action: WorkflowAction::Pause
            }]
        );
        assert_eq!(
            h.handle(Command::WfStop, None, 20),
            vec![HostRequest::SteerWorkflow {
                id: "run-b".into(),
                action: WorkflowAction::Stop
            }]
        );
        assert_eq!(
            h.handle(Command::WfRerun, None, 20),
            vec![HostRequest::SteerWorkflow {
                id: "run-b".into(),
                action: WorkflowAction::Rerun
            }]
        );
        assert_eq!(
            h.handle(Command::WfSave, None, 20),
            vec![HostRequest::SaveWorkflow("run-b".into())]
        );
        // Opened, they act on the OPEN run rather than on whatever row the list
        // cursor was left on.
        h.handle(Command::PanelConfirm, None, 20);
        h.set_workflow_detail(Some(wf_fx::detail()));
        assert_eq!(
            h.handle(Command::WfStop, None, 20),
            vec![HostRequest::SteerWorkflow {
                id: "run-2".into(),
                action: WorkflowAction::Stop
            }]
        );
    }

    #[test]
    fn the_filter_cycles_and_the_done_filter_folds_in_replays() {
        let mut h = open_on(PanelTab::Workflows);
        h.set_workflows(vec![run("run-2", "running")]);
        h.handle(Command::PanelConfirm, None, 20);
        h.set_workflow_detail(Some(wf_fx::detail()));
        h.wf_level = 2;
        assert_eq!(h.shown_agents().len(), 4, "the Review phase, unfiltered");
        for expected in ["running", "queued", "done", "error"] {
            h.handle(Command::WfFilter, None, 20);
            assert_eq!(h.wf_filter(), Some(expected));
        }
        // "done" folded in the two cached calls when it came round.
        h.wf_filter = 3;
        assert_eq!(h.shown_agents().len(), 3, "1 done + 2 cached");
        h.handle(Command::WfFilter, None, 20);
        h.handle(Command::WfFilter, None, 20);
        assert_eq!(h.wf_filter(), None, "the cycle comes back round to all");
    }

    #[test]
    fn o_opens_an_agents_session_and_refuses_when_the_call_was_replayed() {
        let mut h = open_on(PanelTab::Workflows);
        h.set_workflows(vec![run("run-2", "running")]);
        h.handle(Command::PanelConfirm, None, 20);
        h.set_workflow_detail(Some(wf_fx::detail()));
        h.wf_level = 2;
        // Agent `a` is a REPLAY: no session, and the refusal says why rather
        // than opening nothing.
        assert!(h.handle(Command::WfOpenAgent, None, 20).is_empty());
        assert_eq!(h.message.as_deref(), Some(NO_AGENT_SESSION));
        h.agent_sel = 2; // `c`, a live call
        assert_eq!(
            h.handle(Command::WfOpenAgent, None, 20),
            vec![HostRequest::OpenAgentSession("sess-c".into())]
        );
        assert!(!h.open(), "opening a session closes the panel");
    }

    #[test]
    fn a_digit_opens_a_run_from_the_list_and_never_from_a_detail_level() {
        let mut h = open_on(PanelTab::Workflows);
        h.set_workflows(vec![
            run("r0", "done"),
            run("r1", "done"),
            run("r2", "done"),
        ]);
        assert_eq!(
            h.handle(Command::PanelPick, Some(3), 20),
            vec![HostRequest::LoadWorkflow("r2".into())]
        );
        assert_eq!(h.sel, 2);
        // Opened, a digit would open a run other than the one on screen.
        h.set_workflow_detail(Some(wf_fx::detail()));
        h.wf_level = 1;
        assert!(h.handle(Command::PanelPick, Some(1), 20).is_empty());
        assert_eq!(h.wf_level, 1);
    }

    // ---- the mcp tab -------------------------------------------------------

    fn mcp_host() -> PanelHost {
        let mut h = PanelHost::default();
        h.handle(Command::PanelToggle, None, 20);
        h.handle(Command::Tab(PanelTab::Mcp), None, 20);
        h.set_mcp(Some(mcp_fx::status(
            &[
                ("alpha", mcp_fx::stdio("alpha-server")),
                ("beta", mcp_fx::remote("https://b.example/mcp")),
            ],
            &["alpha"],
            &[("beta", false)],
            vec![],
        )));
        h
    }

    #[test]
    fn entering_the_mcp_tab_refetches_it_every_time_never_from_a_cache() {
        let mut h = PanelHost::default();
        h.handle(Command::PanelToggle, None, 20);
        assert_eq!(
            h.handle(Command::Tab(PanelTab::Mcp), None, 20),
            vec![HostRequest::LoadMcp]
        );
        h.set_mcp(Some(mcp_fx::status(&[], &[], &[], vec![])));
        h.handle(Command::Tab(PanelTab::Tree), None, 20);
        // Leaving and returning asks AGAIN: grants and connections change
        // between turns, and last minute's state is worse than none.
        assert_eq!(
            h.handle(Command::Tab(PanelTab::Mcp), None, 20),
            vec![HostRequest::LoadMcp]
        );
    }

    #[test]
    fn enter_toggles_the_grant_in_the_direction_the_row_is_not() {
        let mut h = mcp_host();
        // `alpha` is granted → ⏎ revokes.
        assert_eq!(
            h.handle(Command::PanelConfirm, None, 20),
            vec![HostRequest::SetMcpEnabled {
                name: "alpha".into(),
                enabled: false
            }]
        );
        h.handle(Command::MoveDown, None, 20);
        // `beta` is not → ⏎ grants.
        assert_eq!(
            h.handle(Command::PanelConfirm, None, 20),
            vec![HostRequest::SetMcpEnabled {
                name: "beta".into(),
                enabled: true
            }]
        );
    }

    #[test]
    fn every_mcp_verb_reaches_its_route_for_the_row_under_the_cursor() {
        let mut h = mcp_host();
        h.handle(Command::MoveDown, None, 20); // beta
        assert_eq!(
            h.handle(Command::McpConnect, None, 20),
            vec![HostRequest::ConnectMcpServer("beta".into())]
        );
        assert_eq!(
            h.handle(Command::McpRestart, None, 20),
            vec![HostRequest::RestartMcpServer("beta".into())]
        );
        assert_eq!(
            h.handle(Command::McpAuth, None, 20),
            vec![HostRequest::BeginMcpAuth("beta".into())]
        );
        assert_eq!(
            h.handle(Command::McpForget, None, 20),
            vec![HostRequest::ClearMcpAuth("beta".into())]
        );
    }

    #[test]
    fn deleting_a_registration_takes_two_presses_and_names_what_it_drops() {
        let mut h = mcp_host();
        assert!(
            h.handle(Command::McpRemove, None, 20).is_empty(),
            "the first press only arms"
        );
        let armed = h.message.clone().unwrap_or_default();
        assert!(armed.contains("d again deletes"), "{armed}");
        assert!(armed.contains("credentials are kept"), "{armed}");
        assert_eq!(
            h.handle(Command::McpRemove, None, 20),
            vec![HostRequest::DeleteMcpServer("alpha".into())]
        );
        // …and moving the cursor drops the arm rather than retargeting it.
        let mut h = mcp_host();
        h.handle(Command::McpRemove, None, 20);
        h.handle(Command::MoveDown, None, 20);
        assert!(h.handle(Command::McpRemove, None, 20).is_empty());
        assert_eq!(h.mcp_pending_delete.as_deref(), Some("beta"));
    }

    #[test]
    fn n_opens_a_url_buffer_and_enter_registers_a_name_derived_from_it() {
        let mut h = mcp_host();
        h.handle(Command::McpAdd, None, 20);
        assert_eq!(h.mcp_entry.as_deref(), Some(""));
        for c in "https://mcp.linear.app/sse".chars() {
            h.type_filter(c);
        }
        assert_eq!(
            h.confirm(0),
            vec![HostRequest::AddMcpServer {
                name: "linear".into(),
                url: "https://mcp.linear.app/sse".into()
            }]
        );
        // Something that is not a URL registers nothing and says so.
        let mut h = mcp_host();
        h.handle(Command::McpAdd, None, 20);
        for c in "linear".chars() {
            h.type_filter(c);
        }
        assert!(h.confirm(0).is_empty());
        assert_eq!(h.message.as_deref(), Some(NOT_A_SERVER_URL));
        // esc closes the buffer without registering.
        let mut h = mcp_host();
        h.handle(Command::McpAdd, None, 20);
        h.handle(Command::PanelClose, None, 20);
        assert!(h.mcp_entry.is_none());
        assert!(h.open(), "the buffer went, not the panel");
    }

    #[test]
    fn a_server_name_is_derived_from_its_url_without_the_mcp_and_the_tld() {
        assert_eq!(name_from_url("https://mcp.linear.app/sse", &[]), "linear");
        assert_eq!(name_from_url("https://mcp.notion.com/mcp", &[]), "notion");
        assert_eq!(
            name_from_url("https://api.githubcopilot.com/mcp/", &[]),
            "githubcopilot"
        );
        assert_eq!(name_from_url("https://example.com", &[]), "example");
        // A port and a path change nothing — the host is the whole answer.
        assert_eq!(name_from_url("http://localhost:3000/mcp", &[]), "localhost");
        // Taking "the part before the TLD" naively would call this one "co".
        assert_eq!(name_from_url("https://mcp.acme.co.uk/sse", &[]), "acme");
        // Overwriting silently would replace a registration that may already
        // hold credentials — the one outcome worse than asking.
        assert_eq!(
            name_from_url("https://mcp.linear.app/sse", &["linear".into()]),
            "linear-2"
        );
        assert_eq!(
            name_from_url(
                "https://mcp.linear.app/sse",
                &["linear".into(), "linear-2".into()]
            ),
            "linear-3"
        );
        // Not a URL: no name, so nothing is registered.
        assert_eq!(name_from_url("linear", &[]), "");
        assert_eq!(name_from_url("", &[]), "");
        assert_eq!(name_from_url("https://", &[]), "");
    }

    // ---- the skills tab ----------------------------------------------------

    fn skill(name: &str, description: &str) -> SkillRow {
        SkillRow {
            name: name.into(),
            description: description.into(),
            error: None,
            mcp: Vec::new(),
        }
    }

    #[test]
    fn entering_the_skills_tab_fetches_the_full_rows() {
        let mut h = PanelHost::default();
        h.handle(Command::PanelToggle, None, 20);
        assert_eq!(
            h.handle(Command::Tab(PanelTab::Skills), None, 20),
            vec![HostRequest::LoadSkillRows]
        );
    }

    #[test]
    fn a_failed_skills_fetch_is_none_with_a_reason_never_an_empty_list() {
        let mut h = open_on(PanelTab::Skills);
        h.set_skills(None, Vec::new(), Some("the server did not answer".into()));
        assert!(h.skills.is_none());
        assert_eq!(h.skills_note.as_deref(), Some("the server did not answer"));
        assert!(h.filtered_skills().is_empty());
    }

    #[test]
    fn the_slash_filter_narrows_the_same_list_the_digits_address() {
        let mut h = open_on(PanelTab::Skills);
        h.set_skills(
            Some(vec![
                skill("history", "query the db"),
                skill("wiki", "the personal wiki"),
            ]),
            Vec::new(),
            None,
        );
        h.handle(Command::PanelFilter, None, 20);
        for c in "wik".chars() {
            h.type_filter(c);
        }
        let shown = h.filtered_skills();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].name, "wiki");
        // …and a digit affirms the row that is on screen, not the one that was.
        h.handle(Command::PanelPick, Some(1), 20);
        assert_eq!(
            h.message.as_deref(),
            Some("type /wiki in the composer to load it")
        );
        // esc clears the buffer and the narrowing with it.
        h.handle(Command::PanelFilterExit, None, 20);
        assert_eq!(h.filtered_skills().len(), 2);
    }

    // ---- the model tab -----------------------------------------------------

    /// The nth frontier row: its index among ALL entries (what `sel` counts)
    /// and its model id.
    fn nth_frontier(h: &PanelHost, n: usize) -> (usize, String) {
        match h
            .model_entries()
            .into_iter()
            .enumerate()
            .filter(|(_, e)| e.tier() == crate::components::panel::model::Tier::Frontier)
            .nth(n)
            .expect("a frontier row")
        {
            (at, crate::components::panel::model::ModelEntry::Model { id, .. }) => (at, id),
            _ => unreachable!("filtered to a model row"),
        }
    }

    fn model_host() -> PanelHost {
        let mut h = PanelHost::default();
        h.handle(Command::PanelToggle, None, 20);
        h.handle(Command::Tab(PanelTab::Model), None, 20);
        h.set_models(model_fx::catalog());
        h.set_model_config(model_fx::cfg());
        h
    }

    #[test]
    fn entering_the_model_tab_asks_for_the_catalog_and_the_settings() {
        let mut h = PanelHost::default();
        h.handle(Command::PanelToggle, None, 20);
        assert_eq!(
            h.handle(Command::Tab(PanelTab::Model), None, 20),
            vec![HostRequest::LoadModels, HostRequest::LoadModelSettings]
        );
    }

    #[test]
    fn choosing_a_frontier_row_pins_this_session_and_moves_the_default() {
        let mut h = model_host();
        // Row 2 is `openai:gpt-5-mini` in the frontier section.
        let requests = h.handle(Command::PanelPick, Some(2), 30);
        assert_eq!(
            h.model_cfg.session_model.as_deref(),
            Some("openai:gpt-5-mini")
        );
        assert_eq!(h.model_cfg.default_model, "openai:gpt-5-mini");
        assert_eq!(requests, vec![HostRequest::SaveModel(h.model_cfg.clone())]);
    }

    #[test]
    fn a_cheap_pick_is_refused_in_words_rather_than_moving_a_dot_over_nothing() {
        let mut h = model_host();
        let entries = h.model_entries();
        let at = entries
            .iter()
            .position(|e| e.tier() == crate::components::panel::model::Tier::Cheap)
            .expect("a cheap row");
        let picked = match &entries[at] {
            crate::components::panel::model::ModelEntry::Model { id, .. } => id.clone(),
            _ => unreachable!("filtered to a model row"),
        };
        // The ● and the write are ONE event. This tab used to move the dot to
        // the picked row and then refuse the write in a message beside it,
        // which is a control that reports a change it did not make.
        assert_eq!(
            h.confirm(at),
            vec![HostRequest::SaveModel(h.model_cfg.clone())]
        );
        assert_eq!(h.model_cfg.cheap_model.as_deref(), Some(picked.as_str()));
        assert_eq!(h.message, None, "nothing to apologise for — it saved");
    }

    #[test]
    fn a_cheap_pick_moves_only_the_cheap_tier_never_the_frontier_or_the_effort() {
        let mut h = model_host();
        let before = h.model_cfg.clone();
        let entries = h.model_entries();
        let at = entries
            .iter()
            .position(|e| e.tier() == crate::components::panel::model::Tier::Cheap)
            .expect("a cheap row");
        h.confirm(at);
        // One background model per install, and nothing else moves with it —
        // in particular no session pin, because the cheap tier has none.
        assert_eq!(h.model_cfg.session_model, before.session_model);
        assert_eq!(h.model_cfg.default_model, before.default_model);
        assert_eq!(h.model_cfg.session_effort, before.session_effort);
        assert_eq!(h.model_cfg.default_effort, before.default_effort);
        assert_ne!(h.model_cfg.cheap_model, before.cheap_model);
    }

    #[test]
    fn the_model_tab_opens_on_the_model_in_force_not_on_the_first_row() {
        // The catalog runs to hundreds of rows. Opening at 0 meant the tab that
        // exists to answer "which model is this?" showed a screen of models
        // that were not it, with the ● somewhere below the fold.
        let mut h = model_host();
        h.handle(Command::Tab(PanelTab::Model), None, 20);
        let (at, id) = nth_frontier(&h, 1);
        assert_ne!(at, 0, "the fixture must not make this vacuous");
        let mut cfg = h.model_cfg.clone();
        cfg.session_model = Some(id.clone());
        cfg.default_model = id;
        h.set_model_config(cfg);
        assert_eq!(h.sel, at);
    }

    #[test]
    fn a_late_settings_answer_does_not_yank_a_cursor_already_being_moved() {
        let mut h = model_host();
        h.handle(Command::Tab(PanelTab::Model), None, 20);
        h.handle(Command::MoveDown, None, 20);
        let moved = h.sel;
        let (at, id) = nth_frontier(&h, 0);
        assert_ne!(at, moved, "the fixture must not make this vacuous");
        let mut cfg = h.model_cfg.clone();
        cfg.session_model = Some(id.clone());
        cfg.default_model = id;
        h.set_model_config(cfg);
        assert_eq!(h.sel, moved, "the first cursor move disarms the landing");
    }

    #[test]
    fn choosing_an_effort_row_never_writes_itself_into_a_model_field() {
        let mut h = model_host();
        let entries = h.model_entries();
        let at = entries
            .iter()
            .position(|e| e.tier() == crate::components::panel::model::Tier::Effort)
            .expect("an effort row")
            + 4; // "xhigh"
        h.confirm(at);
        assert_eq!(
            h.model_cfg.default_model, "claude-opus-5",
            "the model field is untouched"
        );
        assert_eq!(h.model_cfg.session_effort.map(|e| e.id()), Some("xhigh"));
    }

    #[test]
    fn the_two_search_boxes_narrow_their_own_tier_and_tab_crosses_between_them() {
        let mut h = model_host();
        h.handle(Command::PanelFilter, None, 30);
        assert_eq!(h.model_focus, Some(Tier::Frontier));
        for c in "mini".chars() {
            h.type_filter(c);
        }
        assert_eq!(h.model_filters.frontier, "mini");
        assert_eq!(h.model_filters.cheap, "", "the other box is untouched");
        h.handle(Command::PanelFilterTier, None, 30);
        assert_eq!(h.model_focus, Some(Tier::Cheap));
        for c in "opus".chars() {
            h.type_filter(c);
        }
        assert_eq!(h.model_filters.cheap, "opus");
        assert_eq!(h.model_filters.frontier, "mini");
        // ⌫ edits the FOCUSED box only.
        h.handle(Command::PanelFilterBack, None, 30);
        assert_eq!(h.model_filters.cheap, "opu");
        assert_eq!(h.model_filters.frontier, "mini");
    }

    #[test]
    fn a_filter_buffer_belongs_to_one_tab_and_narrows_nothing_else() {
        // An MCP URL half-typed must never narrow the conversation list
        // underneath it, and a skills query must not narrow the tree.
        let mut h = open_on(PanelTab::Skills);
        h.set_skills(Some(vec![skill("history", "x")]), Vec::new(), None);
        h.handle(Command::PanelFilter, None, 20);
        for c in "zzz".chars() {
            h.type_filter(c);
        }
        assert!(h.filtered_skills().is_empty());
        h.handle(Command::Tab(PanelTab::Model), None, 20);
        // The tree's buffer is not the model's boxes.
        assert_eq!(h.model_filters.frontier, "");
        assert_eq!(h.model_filters.cheap, "");
    }

    // ---- the tabs' letters are disjoint ------------------------------------

    /// The gate the keymap's own `dead_bindings` proves structurally, asserted
    /// here from the TAB's side: two tabs may share a letter only because the
    /// keymap scopes each row to its own tab. A letter bound in a tab is
    /// delivered to THAT tab and to no other.
    #[test]
    fn a_bare_letter_reaches_only_the_tab_that_claims_it() {
        // `r` is `wf.rerun` in workflows and `mcp.restart` in mcp.
        let mut wf = open_on(PanelTab::Workflows);
        wf.set_workflows(vec![run("run-1", "done")]);
        assert_eq!(
            wf.handle(Command::WfRerun, None, 20),
            vec![HostRequest::SteerWorkflow {
                id: "run-1".into(),
                action: WorkflowAction::Rerun
            }]
        );
        // …and the MCP verb, delivered while the workflows tab is open, does
        // nothing rather than acting on a row of another tab's list.
        assert!(wf.handle(Command::McpRestart, None, 20).is_empty());
        let mut mcp = mcp_host();
        assert_eq!(
            mcp.handle(Command::McpRestart, None, 20),
            vec![HostRequest::RestartMcpServer("alpha".into())]
        );
        assert!(mcp.handle(Command::WfRerun, None, 20).is_empty());
        // `x` is `wf.stop` and `changes.revert`; `e` is `wf.script` and
        // `tree.extract`.
        let mut tree = open_on(PanelTab::Tree);
        assert!(tree.handle(Command::WfStop, None, 20).is_empty());
        assert!(tree.handle(Command::WfScript, None, 20).is_empty());
    }

    #[test]
    fn every_tab_answers_its_own_row_count_so_the_cursor_has_something_to_clamp_to() {
        for tab in crate::keys::PANEL_TABS {
            let mut h = open_on(tab);
            // Moving on an empty tab is a no-op, never a panic or a cursor past
            // the end.
            h.handle(Command::MoveDown, None, 20);
            h.handle(Command::MoveUp, None, 20);
            assert_eq!(h.sel, 0, "{tab:?}");
        }
    }
}
