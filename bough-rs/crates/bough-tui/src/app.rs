//! The TUI event loop (wave-1 port of `src/tui/components/App.tsx` +
//! `src/tui/main.tsx` responsibilities, row 1.39).
//!
//! Concurrency shape (ARCHITECTURE §5): the crossterm `EventStream`, the SSE
//! task and the timer tasks all post [`Action`]s over ONE mpsc; the reducer
//! ([`App::apply`]) runs on the single loop task and stays pure of I/O — every
//! outbound call is an [`Effect`] handed to the injected [`Transport`], so the
//! whole loop is scriptable in tests with no terminal and no server attached.
//!
//! SCOPE (kept honest, per PORT_PLAN and spec §8 v1 cut): chat, the one
//! tabbed panel (tree + changes; the other tabs say so), the help overlay, the
//! live-work rail (`components/rail.rs`) and the job view
//! (`components/job_output.rs`) behind it, the ask card
//! (`components/ask.rs`) and the take-back window (`store/lifecycle.rs`) are
//! wired. The rail's feed is the jobs poll, which rides the spinner timer:
//! the loop's idle-tick skip therefore asks [`App::animating`], not
//! `busy()` — under `busy()` alone a shell that outlives its turn could never
//! repaint. STILL UNWIRED: workflow and schedule rail rows (no feed for them
//! in this client yet — `live_units` renders their absence as no rows).
//! Ghost absent (cheap tier is `None`);
//! mouse = wheel scroll + click; the `!` shell answers "not wired into this
//! client" and keeps the draft rather than billing the model. The
//! transcript builder below is a deliberate v1 miniature of `lines.rs`
//! (row 1.37, in flight); it renders prose, folded thinking/tool headers and
//! the streaming `▌` cursor, and is replaced wholesale when `buildLines`
//! lands. Behavior contracts preserved: streaming render, esc interrupts a
//! running turn (esc esc never shadows the stop), scrollback counts up from
//! the live tail, ^c is a two-press quit.

use std::collections::HashMap;

use bough_core::schema::events::{
    BoughEvent, EventType, MessageDeltaData, MessageFinishedData, MessagePartData,
    MessageRetryData, SessionActivityData, TurnFinishedData,
};
use bough_core::schema::parts::{
    AskQuestion, AskQuestionStatus, BackgroundJob, Message, Part, Role,
};
use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::components::ask::{ask_card_height, ask_prompt_lines, render_ask_card, AskCardProps};
use crate::components::chat::{render_chat, ChatProps, CHAT_PLACEHOLDER};
use crate::components::job_output::{
    job_body_rows, job_sub_lines, render_job_output, JobOutputProps,
};
use crate::components::rail::{live_subagents, rail_rows, render_rail};
use crate::components::composer::{
    completion_popup_height, composer_height, render_completion_popup, render_composer,
    CompletionPopupProps, ComposerProps,
};
use crate::components::help::{clamp_help_offset, overlay_lines, render_help, HELP_STEP};
use crate::components::panel::host::{HostRequest, PanelHost};
use crate::components::panel::{panel_body_rows, render_panel, PanelBody};
use crate::components::status::{render_status, ChatMeter};
use crate::components::WARN;
use crate::format::{
    active_trigger, apply_completion, browse_prefix, rank_completions, Candidate, Ranked, Trigger,
    TriggerKind, COMPLETION_LIMIT,
};
use crate::keys::{
    lookup, slash_invocation, tab_for_command, unknown_command, Command, KeyContext, KeyFlags,
    UiMode, SLASH_COMMANDS,
};
use crate::selection::{
    is_empty_selection, link_at, row_content, selected_copy, url_at, CopyRow, Point, Selection,
};
use crate::store::selectors::{live_units, LiveUnit, LiveUnitKind};

/// App.tsx::DOUBLE_ESC_MS — the double-tap window.
pub const DOUBLE_ESC_MS: i64 = 600;
/// App.tsx::WHEEL_ROWS — transcript rows per wheel tick.
pub const WHEEL_ROWS: usize = 3;

/// Options the composition root passes in (main.tsx: `-w` else cwd).
#[derive(Default, Clone)]
pub struct TuiOptions {
    pub workspace: Option<String>,
}

/// Everything that can reach the reducer — all tasks post these over one mpsc.
pub enum Action {
    Term(TermEvent),
    /// One SSE envelope off the socket.
    Event(BoughEvent),
    /// The SSE stream connected / dropped.
    Connected(bool),
    /// Spinner/elapsed clock, at `SPINNER_MS` while busy.
    Tick,
    /// The transport created (or adopted) the conversation this screen shows.
    SessionOpened(String),
    /// A transport failure worth a row (an `ApiFailure`'s own sentence).
    Notice(String),
    /// The `@` candidate list for this conversation's workspace, gitignore-
    /// filtered at the source (`git ls-files`).
    Files(Vec<String>),
    /// One directory's entries, for an `@` path that left the workspace. The
    /// prefix travels with them: a reply for a path the user has already typed
    /// past must not rank against the query it does not belong to.
    DirEntries { prefix: String, entries: Vec<String> },
    /// The installed skills, for the `/` popup's rows below the built-ins.
    Skills(Vec<(String, String)>),
    /// `GET /sessions` — the tree tab's rows.
    Sessions(Vec<crate::api::SessionRow>),
    /// `GET /sessions/:id/changes`. `None` is a failed fetch, not an empty set.
    Changes(Option<crate::store::state::SessionChangeSet>),
    /// `GET /theme` — the palette in force, seeding the theme tab's baseline.
    /// `None` is a failed fetch: the preview still opens, on the built-in
    /// palette, because a picker that cannot open is worse than one that
    /// browses from the default.
    Theme(Option<crate::theme::ThemeState>),
    /// The opened conversation's thread, from `GET /sessions/:id`.
    Thread(Vec<Message>),
    /// A built-in `/command` coming back from the transport, for the surfaces
    /// this client owns (the panel's tabs, the help overlay). A slash command
    /// dispatches as an [`Effect`] so the send path stays one funnel; the ones
    /// the client answers itself return here.
    Run(Command),
    /// `GET /sessions/:id/jobs` — the shells running on this conversation's
    /// behalf (its subagents' included). The rail is built from these.
    Jobs(Vec<BackgroundJob>),
    /// One job's whole retained buffer, from the open job view's own poll.
    /// `error` replaces the buffer rather than closing the view: a job whose
    /// row went away still has an id worth saying.
    JobOutput {
        id: String,
        output: String,
        job: Option<BackgroundJob>,
        error: Option<String>,
    },
    /// The live `ask()` holds (`GET /questions`), for a client that attached
    /// after the hold was raised — the server keeps them in memory only.
    Asks(Vec<AskQuestion>),
    /// A posted take-back came back: the text returns to the composer.
    TookBack(String),
}

/// Outbound calls. The loop never does I/O itself; the transport does.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// POST the draft as a user message.
    Send(String),
    /// POST /sessions/:id/interrupt.
    Interrupt,
    /// GET the `@` candidates for this conversation (or, before one exists,
    /// for the workspace it would start in — that is the screen where someone
    /// first types `@`, and an empty popup there reads as broken).
    LoadFiles,
    /// GET one directory's entries for an `@` path that leaves the workspace.
    LoadDirEntries(String),
    /// GET the installed skills, once.
    LoadSkills,
    /// A built-in `/command` the user dispatched — from the popup, or from a
    /// draft that IS one at send time. Never sent to the model.
    Run(Command, String),
    /// GET the conversations for the tree tab.
    LoadSessions,
    /// GET the open conversation's change set for the changes tab.
    LoadChanges,
    /// Open this conversation (⏎ on a tree row): the switcher's whole point.
    OpenSession(String),
    /// POST the armed revert. `None` = the whole set; the server refuses `[]`,
    /// so an empty list is never sent.
    Revert(Option<Vec<String>>),
    /// GET the palette in force, on entry to the theme tab.
    LoadTheme,
    /// Persist the kept palette — PUT or DELETE, as `persist_request` decided.
    SaveTheme(crate::theme::ThemeWrite),
    /// GET the jobs for the open conversation — the rail's whole feed.
    PollJobs,
    /// GET one job's retained buffer (non-destructive: watching a job never
    /// eats output the model's next `bashOutput` was owed).
    LoadJobOutput(String),
    /// POST the kill for one job.
    KillJob(String),
    /// Stop one delegated session (a rail row that is an agent, not a shell).
    StopSession(String),
    /// GET the live `ask()` holds, on attach and on session switch.
    LoadQuestions,
    /// Answer / decline the hold the card is showing.
    AnswerAsk { session_id: String, id: String, answer: String },
    DeclineAsk { session_id: String, id: String },
    /// The posted take-back: delete this message (and what followed) and stop
    /// the turn it started, in ONE call.
    Unsend(String),
}

/// The transport seam — scripted in tests; wired to `api.rs` when row 1.32 lands.
pub trait Transport {
    fn effect(&mut self, effect: Effect);
}

impl<F: FnMut(Effect)> Transport for F {
    fn effect(&mut self, effect: Effect) {
        self(effect)
    }
}

struct TurnClock {
    started_at: i64,
    ended: bool,
}

/// The open job view's own state. The buffer is a prop the poll refreshes;
/// nothing here is derived from the rail, because a job that EXITS while you
/// are reading it must not close the view under you.
struct JobView {
    id: String,
    output: String,
    job: Option<BackgroundJob>,
    error: Option<String>,
    /// Lines up from the tail; 0 follows live output.
    scroll: usize,
    /// `x` armed a kill — the footer says what the next press does.
    armed: bool,
}

/// How often the rail re-reads what is running, in spinner ticks
/// (`SPINNER_MS` = 120ms → ~1s). The rail redraws every second in the TS, and
/// this is the same cadence expressed in the one timer this loop already has.
const POLL_TICKS: u64 = 8;

/// The take-back's on-screen affordance. The TS gesture is keymap-only, which
/// makes a three-second window that nothing announces; this row is the window,
/// said out loud, and it expires with it.
pub const TAKE_BACK_HINT: &str = "esc takes that back";

/// The whole UI state, mutated only by [`App::apply`] on the loop task.
pub struct App<T: Transport> {
    transport: T,
    options: TuiOptions,
    cols: u16,
    rows: u16,
    connected: bool,
    /// The open conversation. None until the first send creates one.
    session_id: Option<String>,
    /// Draft text; `cursor` is a CHAR index into it (keys.ts contract).
    draft: String,
    cursor: usize,
    /// Lines up from the live tail; 0 follows output.
    scroll_off: usize,
    notice: Option<String>,
    quit_armed: bool,
    pub quit: bool,
    thread: Vec<Message>,
    /// message id → streamed-but-unfinalized text.
    streaming: HashMap<String, String>,
    activity: Option<String>,
    turn: Option<TurnClock>,
    tick: u64,
    last_esc_at: Option<i64>,
    now_ms: i64,
    /// Local send counter for optimistic user-message echo ids.
    sent_seq: u64,
    // ---- the @// completion ------------------------------------------------
    /// `@` candidates: the workspace listing, once per conversation.
    files: Vec<String>,
    /// `/` candidates below the built-ins: (name, description).
    skills: Vec<(String, String)>,
    /// The directory listing behind an `@` path that left the workspace, and
    /// the prefix it answers for.
    browsed: (String, Vec<String>),
    /// Cursor within the popup rows.
    completion_sel: usize,
    /// Esc dismissed THIS token's popup — typing re-opens it, so esc means esc
    /// without meaning "no completion ever again".
    dismissed: bool,
    // ---- mouse selection (rows 2.25) ---------------------------------------
    /// The open drag, in SCREEN coordinates. A drag is a gesture on the screen,
    /// and storing it against the transcript would make it slide when new
    /// output arrives underneath it, highlighting text nobody selected.
    sel: Option<Selection>,
    /// Where a copy goes. Injected so a test never writes to a terminal; the
    /// production writer is OSC 52, which reaches the LOCAL clipboard over
    /// ssh/tmux (term.rs).
    copy: Box<dyn Fn(&str) + Send>,
    /// Where a clicked link goes. `open`/`xdg-open`, detached, failures
    /// ignored — and http(s) ONLY, because transcript URLs are model-written
    /// (main.tsx calls this a security boundary).
    open: Box<dyn Fn(&str) + Send>,
    /// One fetch per fact, not one per keystroke.
    files_requested: bool,
    skills_requested: bool,
    browse_requested: Option<String>,
    // ---- the one panel, and the overlay (row 2.20) -------------------------
    /// The panel's whole state: which tab, the cursor, the tree's expansion,
    /// the change set and what a revert has armed.
    panel: PanelHost,
    /// The help overlay is the ONE surface that displaces everything.
    help_open: bool,
    help_off: usize,
    // ---- the live-work rail and the job view (row 2.19) --------------------
    /// The shells running for this conversation and its delegates.
    jobs: Vec<BackgroundJob>,
    /// The rail's cursor. `None` = the composer has the keyboard and the rail
    /// still renders — ↓ into it is reversible, not a mode switch.
    rail_sel: Option<usize>,
    /// The unit `x` has armed. Consent is never inferred.
    rail_armed: Option<String>,
    /// The open job, if any.
    job: Option<JobView>,
    /// Ticks since the last poll; the rail's feed rides the spinner timer.
    poll_tick: u64,
    // ---- the ask card and the take-back window (row 2.21) ------------------
    /// The hold the card is showing. A pending `ask()` OWNS the keyboard.
    ask: Option<AskQuestion>,
    /// The free-text answer, as typed.
    ask_typed: String,
    /// When the last message left this client — the take-back window's clock.
    /// Read at the keystroke rather than held as a flag: the window expires on
    /// the clock, and a flag would need a timer that can be missed.
    last_send_at: Option<i64>,
}

impl<T: Transport> App<T> {
    pub fn new(options: TuiOptions, transport: T, cols: u16, rows: u16) -> Self {
        Self {
            transport,
            options,
            cols: cols.max(20),
            rows: rows.max(8),
            connected: false,
            session_id: None,
            draft: String::new(),
            cursor: 0,
            scroll_off: 0,
            notice: None,
            quit_armed: false,
            quit: false,
            thread: Vec::new(),
            streaming: HashMap::new(),
            activity: None,
            turn: None,
            tick: 0,
            last_esc_at: None,
            now_ms: 0,
            sent_seq: 0,
            files: Vec::new(),
            skills: Vec::new(),
            browsed: (String::new(), Vec::new()),
            completion_sel: 0,
            sel: None,
            copy: Box::new(osc52_copy),
            open: Box::new(open_url),
            dismissed: false,
            files_requested: false,
            skills_requested: false,
            browse_requested: None,
            panel: PanelHost::default(),
            help_open: false,
            help_off: 0,
            jobs: Vec::new(),
            rail_sel: None,
            rail_armed: None,
            job: None,
            poll_tick: 0,
            ask: None,
            ask_typed: String::new(),
            last_send_at: None,
        }
    }

    /// A turn is in flight iff any message is pending (store.ts::isBusy).
    pub fn busy(&self) -> bool {
        self.thread.iter().any(|m| m.pending)
    }

    /// Anything on screen that moves on its own — what an idle tick may repaint
    /// for. The busy check alone made the jobs poll unpaintable: a shell that
    /// starts while the turn is over has nothing else to bring it on screen.
    pub fn animating(&self) -> bool {
        self.busy() || !self.jobs.is_empty() || self.job.is_some() || self.just_sent()
    }

    /// Is a send still inside the take-back window? Decided by the ported rule
    /// (`store::lifecycle::just_sent`) over the one field it reads, so the
    /// window's length lives in exactly one place (`keys::UNSEND_MS`).
    pub fn just_sent(&self) -> bool {
        let probe = crate::store::state::TuiState {
            last_send_at: self.last_send_at,
            ..crate::store::state::initial_state()
        };
        crate::store::lifecycle::just_sent(&probe, self.now_ms)
    }

    // ---- the live-work rail (row 2.19) -------------------------------------

    /// Everything running on this conversation's behalf, as rows. Rebuilt per
    /// read from the polled facts — the numbers a stop acts on are the numbers
    /// on screen. Workflows and schedules are absent from this client (no feed
    /// for them yet), and `live_units` renders their absence as no rows.
    fn units(&self) -> Vec<LiveUnit> {
        let Some(current) = self.session_id.as_deref() else {
            return Vec::new();
        };
        let children: Vec<crate::api::SessionRow> = self
            .panel
            .sessions
            .iter()
            .filter(|s| {
                s.session.parent_id.as_deref() == Some(current)
                    || s.session.origin_id.as_deref() == Some(current)
            })
            .cloned()
            .collect();
        // `rail.rs` was ported against `api::SessionRow` and `selectors.rs`
        // against `store::state::SessionRow` — the same wire shape declared
        // twice (state.rs's header says api.rs should absorb it). Adapting at
        // the call site rather than editing either module: the field list is
        // written out, so a divergence is a compile error here.
        let subagents: Vec<crate::store::state::SessionRow> = live_subagents(&children)
            .into_iter()
            .map(|s| crate::store::state::SessionRow {
                session: s.session,
                busy: s.busy,
                last_turn_status: s.last_turn_status,
                cost_usd: s.cost_usd,
                tokens: s.tokens,
            })
            .collect();
        live_units(&self.jobs, &subagents, &[], &[], self.now_ms)
    }

    /// The cursor must never point past a rail that just got shorter — a job
    /// exits while you are on its row, and `x` must not then stop its neighbour.
    fn clamp_rail(&mut self) {
        let len = self.units().len();
        match self.rail_sel {
            Some(_) if len == 0 => {
                self.rail_sel = None;
                self.rail_armed = None;
            }
            Some(at) if at >= len => self.rail_sel = Some(len - 1),
            _ => {}
        }
    }

    /// The rows the rail paints, capped at a third of the screen: the rail is
    /// pinned under the composer and must never push it off.
    fn rail_lines(&self) -> Vec<String> {
        let units = self.units();
        if units.is_empty() {
            return Vec::new();
        }
        let cap = ((self.rows as usize) / 3).max(1);
        let mut rows = rail_rows(
            &units,
            self.rail_sel,
            self.cols.max(20) as usize,
            self.rail_armed.as_deref(),
        );
        rows.truncate(cap);
        rows
    }

    // ---- reducer -----------------------------------------------------------

    /// Apply one action. `now_ms` is injected (main.tsx injects `now` for the
    /// double-esc tests) — the reducer never reads a wall clock.
    pub fn apply(&mut self, action: Action, now_ms: i64) {
        self.now_ms = now_ms;
        match action {
            Action::Tick => {
                if self.busy() {
                    self.tick = self.tick.wrapping_add(1);
                }
                // The take-back row is the window: it goes when the window does.
                if self.notice.as_deref() == Some(TAKE_BACK_HINT) && !self.just_sent() {
                    self.notice = None;
                }
                self.poll_tick = self.poll_tick.wrapping_add(1);
                if self.poll_tick % POLL_TICKS == 0 && self.session_id.is_some() {
                    self.transport.effect(Effect::PollJobs);
                    // The rail's agent rows come from the listing, and a fan-out
                    // that started since the last read is exactly what it is for.
                    self.transport.effect(Effect::LoadSessions);
                    if let Some(job) = &self.job {
                        self.transport.effect(Effect::LoadJobOutput(job.id.clone()));
                    }
                }
            }
            Action::Jobs(jobs) => {
                self.jobs = jobs;
                self.clamp_rail();
            }
            Action::JobOutput { id, output, job, error } => {
                if let Some(view) = self.job.as_mut() {
                    if view.id == id {
                        view.output = output;
                        view.job = job;
                        view.error = error;
                    }
                }
            }
            Action::Asks(asks) => {
                self.ask = asks
                    .into_iter()
                    .find(|q| q.status == AskQuestionStatus::Pending);
            }
            Action::TookBack(text) => {
                self.notice = Some(crate::store::lifecycle::take_back_notice(self.busy()).to_string());
                self.cursor = text.chars().count();
                self.draft = text;
                self.scroll_off = 0;
                self.last_send_at = None;
            }
            Action::Connected(up) => self.connected = up,
            Action::SessionOpened(id) => {
                self.panel.current_id = Some(id.clone());
                self.session_id = Some(id);
                // A session switch invalidates the listing: the candidates are
                // that conversation's workspace, not this one's.
                self.files.clear();
                self.files_requested = false;
                // …and the change set, which is that conversation's checkout.
                self.panel.set_changes(None);
                // …and everything the rail and the card were showing FOR the
                // conversation being left: another session's shells and holds
                // pinned under this composer would be a claim about work that
                // is not this screen's.
                self.jobs.clear();
                self.rail_sel = None;
                self.rail_armed = None;
                self.job = None;
                self.ask = None;
                self.ask_typed.clear();
                self.transport.effect(Effect::PollJobs);
                self.transport.effect(Effect::LoadQuestions);
            }
            Action::Thread(thread) => {
                self.thread = thread;
                self.streaming.clear();
                // A switch lands at the live tail, like every arrival.
                self.scroll_off = 0;
            }
            Action::Sessions(sessions) => self.panel.set_sessions(sessions),
            Action::Changes(set) => self.panel.set_changes(set),
            Action::Theme(state) => self.panel.set_theme(state),
            Action::Run(command) => self.run_client_command(command),
            Action::Files(files) => self.files = files,
            Action::DirEntries { prefix, entries } => self.browsed = (prefix, entries),
            Action::Skills(skills) => self.skills = skills,
            Action::Notice(text) => self.notice = Some(text),
            Action::Event(event) => self.reduce_event(event),
            Action::Term(TermEvent::Resize(w, h)) => {
                self.cols = w.max(20);
                self.rows = h.max(8);
            }
            Action::Term(TermEvent::Mouse(m)) => self.on_mouse(m),
            Action::Term(TermEvent::Key(k)) => self.on_key(k, now_ms),
            Action::Term(_) => {}
        }
    }

    /// Wheel, drag selection and click (row 2.25).
    ///
    /// A press opens a selection rather than acting: which gesture it turns out
    /// to be is not knowable until the button comes back up, because a click and
    /// the first cell of a drag are the same event. On release a real drag COPIES
    /// (the way a terminal's own selection does — requiring a second keystroke to
    /// keep what you just highlighted is a step nobody expects) and a click
    /// hit-tests for a link.
    fn on_mouse(&mut self, m: MouseEvent) {
        // Reports are 0-based; a selection is 1-based cells (selection.rs).
        let at = Point { x: m.column as i64 + 1, y: m.row as i64 + 1 };
        match m.kind {
            MouseEventKind::ScrollUp => self.scroll_by(WHEEL_ROWS as isize),
            MouseEventKind::ScrollDown => self.scroll_by(-(WHEEL_ROWS as isize)),
            MouseEventKind::Down(MouseButton::Left) => {
                self.sel = Some(Selection { anchor: at, focus: at });
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(s) = self.sel.as_mut() {
                    s.focus = at;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let Some(mut s) = self.sel.take() else { return };
                s.focus = at;
                if is_empty_selection(&s) {
                    self.click_at(at);
                    return;
                }
                self.copy_selection(&s);
            }
            _ => {}
        }
    }

    /// The screen as PLAIN rows, read back by re-rendering into a scratch
    /// buffer. The transcript is the only surface this file holds the lines of,
    /// so a drag over the composer or the status row would otherwise copy
    /// nothing at all; answering from the painted grid makes a selection work on
    /// every surface without each one having to hand its rows up.
    fn painted_rows(&self) -> Vec<String> {
        let area = Rect { x: 0, y: 0, width: self.cols.max(20), height: self.rows.max(8) };
        let mut buf = Buffer::empty(area);
        self.draw(area, &mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    /// Copy on release, and say how much. The transcript's own line carries the
    /// unwrapped source a paste should actually contain (`VLine::src`); every
    /// other surface has only what is painted.
    fn copy_selection(&mut self, s: &Selection) {
        let painted = self.painted_rows();
        let text = selected_copy(s, |y| {
            usize::try_from(y - 1)
                .ok()
                .and_then(|i| painted.get(i))
                .map(CopyRow::painted)
        });
        if text.trim().is_empty() {
            return;
        }
        (self.copy)(&text);
        let n = text.chars().count();
        self.notice = Some(format!(
            "copied {n} character{}",
            if n == 1 { "" } else { "s" }
        ));
    }

    /// A click. Links first: a URL under this exact column beats anything the
    /// row belongs to, because a URL is the more specific thing to have aimed
    /// at. TWO READINGS, because there are two kinds of link on screen — the
    /// transcript's OSC 8 markers, and the plain text everything else paints.
    fn click_at(&mut self, at: Point) {
        let painted = self.painted_rows();
        let Some(row) = usize::try_from(at.y - 1).ok().and_then(|i| painted.get(i)) else {
            return;
        };
        let col = (at.x - 1).max(0) as usize;
        if let Some(url) = link_at(row, col) {
            self.open_link(&url);
            return;
        }
        let (content, offset) = row_content(row);
        if let Some(url) = col.checked_sub(offset).and_then(|c| url_at(&content, c)) {
            self.open_link(&url);
        }
    }

    /// http(s) ONLY — a security boundary, not a formality: transcript URLs are
    /// model-written, and `open` will launch anything a scheme is registered for.
    fn open_link(&mut self, url: &str) {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return;
        }
        (self.open)(url);
    }

    /// Test seam: where a copy and an opened link go.
    pub fn set_copy(&mut self, copy: Box<dyn Fn(&str) + Send>) {
        self.copy = copy;
    }
    pub fn set_open(&mut self, open: Box<dyn Fn(&str) + Send>) {
        self.open = open;
    }

    fn scroll_by(&mut self, delta: isize) {
        let max = self.transcript_lines().len().saturating_sub(1);
        let next = self.scroll_off as isize + delta;
        self.scroll_off = next.clamp(0, max as isize) as usize;
    }

    // ---- the @// completion (App.tsx's container half) ---------------------

    /// What the cursor is completing, if anything. Pure — it decides from the
    /// draft alone, which is what lets the `@`/`/` behavior be tested on
    /// strings with no server attached.
    fn trigger(&self) -> Option<Trigger> {
        if self.dismissed {
            return None;
        }
        active_trigger(&self.draft, self.cursor)
    }

    /// The ranked rows for that trigger. The candidate SOURCE is the only part
    /// that varies: a path that leaves the workspace ranks one directory's
    /// entries, everything else ranks the git listing (files) or the built-in
    /// commands then the skills (`/`).
    fn completion(&self) -> Ranked {
        let Some(trigger) = self.trigger() else {
            return Ranked::default();
        };
        if trigger.kind == TriggerKind::File {
            if let Some(prefix) = browse_prefix(&trigger.query) {
                // Until the fetch for THIS prefix lands, the previous
                // directory's entries would rank against a query they do not
                // belong to — an empty popup is the honest state for that beat.
                if self.browsed.0 != prefix {
                    return Ranked::default();
                }
                let candidates: Vec<Candidate> = self
                    .browsed
                    .1
                    .iter()
                    .map(|name| Candidate::file(format!("{prefix}{name}")))
                    .collect();
                return rank_completions(&candidates, &trigger, COMPLETION_LIMIT);
            }
            let candidates: Vec<Candidate> =
                self.files.iter().map(|name| Candidate::file(name.clone())).collect();
            return rank_completions(&candidates, &trigger, COMPLETION_LIMIT);
        }
        // Built-in commands come FIRST in the candidate list, which is also how
        // they win a tie: `rank_completions` breaks equal scores by source
        // order. `/skills` the tab therefore outranks a skill that happens to be
        // called "skills", and the row that acts on the harness is never buried
        // under installed content.
        let mut candidates: Vec<Candidate> = SLASH_COMMANDS
            .iter()
            .map(|c| Candidate::command(c.name, c.desc, c.command))
            .collect();
        candidates.extend(self.skills.iter().map(|(name, desc)| Candidate::skill(name, desc)));
        rank_completions(&candidates, &trigger, COMPLETION_LIMIT)
    }

    /// `completing` in the keymap's guard sense: the popup has rows, so it owns
    /// ⏎/⇥/↑/↓/esc. A trigger that matched nothing still DRAWS (saying so), but
    /// must not swallow the keys.
    fn completing(&self) -> bool {
        !self.completion().items.is_empty()
    }

    /// The cursor, clamped: it must never point past a list that just got
    /// shorter as you typed.
    fn sel_at(&self, len: usize) -> usize {
        if len == 0 {
            0
        } else {
            self.completion_sel.min(len - 1)
        }
    }

    /// One fetch per fact. Lazy on purpose: nothing is listed until the user
    /// types a marker, and then exactly once per conversation (files), per
    /// process (skills) or per browsed directory.
    fn ensure_candidates(&mut self) {
        let Some(trigger) = self.trigger() else { return };
        match trigger.kind {
            TriggerKind::File => match browse_prefix(&trigger.query) {
                Some(prefix) => {
                    if self.browse_requested.as_deref() != Some(prefix.as_str()) {
                        self.browse_requested = Some(prefix.clone());
                        self.transport.effect(Effect::LoadDirEntries(prefix));
                    }
                }
                None => {
                    if !self.files_requested {
                        self.files_requested = true;
                        self.transport.effect(Effect::LoadFiles);
                    }
                }
            },
            TriggerKind::Skill => {
                if !self.skills_requested {
                    self.skills_requested = true;
                    self.transport.effect(Effect::LoadSkills);
                }
            }
        }
    }

    /// ⏎/⇥ on the highlighted row. A built-in `/command` ACTS: the token still
    /// comes out of the draft — leaving `/model ` behind would put it in the
    /// next message as text — but what follows is the command, not an insertion.
    fn complete_accept(&mut self) {
        let Some(trigger) = self.trigger() else { return };
        let ranked = self.completion();
        let Some(item) = ranked.items.get(self.sel_at(ranked.items.len())).cloned() else {
            return;
        };
        self.completion_sel = 0;
        if let Some(command) = item.run {
            let chars: Vec<char> = self.draft.chars().collect();
            let start = trigger.start.min(chars.len());
            let end = trigger.end.min(chars.len()).max(start);
            let head: String = chars[..start].iter().collect();
            let tail: String = chars[end..].iter().collect();
            self.draft = format!("{head}{tail}");
            self.cursor = start;
            self.transport.effect(Effect::Run(command, String::new()));
            return;
        }
        let (text, cursor) = apply_completion(&self.draft, &trigger, &item);
        self.draft = text;
        self.cursor = cursor;
        // A directory keeps its slash, so accepting it re-triggers and drills
        // one level down — the next listing is fetched here, not on the next
        // keystroke, or the popup blinks empty until you type again.
        self.ensure_candidates();
    }

    /// The popup's keys, ahead of the composer's own and ahead of the stop:
    /// escape unwinds exactly ONE level, nearest surface first (keys.rs
    /// BINDINGS put every `complete.*` row before its unguarded fallback).
    /// Returns true when the popup consumed the key.
    fn on_completion_key(&mut self, k: &KeyEvent) -> bool {
        if !self.completing() {
            return false;
        }
        let len = self.completion().items.len();
        match k.code {
            KeyCode::Up => self.completion_sel = self.sel_at(len).saturating_sub(1),
            KeyCode::Down => self.completion_sel = (self.sel_at(len) + 1).min(len - 1),
            KeyCode::Tab => self.complete_accept(),
            KeyCode::Enter
                if !k.modifiers.contains(KeyModifiers::SHIFT)
                    && !k.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.complete_accept()
            }
            // Stays dismissed until the trigger token changes, so esc means esc.
            KeyCode::Esc => self.dismissed = true,
            _ => return false,
        }
        true
    }

    // ---- the one panel and the overlay (row 2.20) --------------------------

    /// The growing region's height — the transcript, or the panel that
    /// displaces it. ONE copy, shared by the renderer and by the key handler:
    /// a second derivation of "how many rows are visible" is how a digit comes
    /// to affirm a row nobody can see.
    fn chat_height(&self, cols: u16, rows: u16, popup_h: u16) -> u16 {
        let input_h = self.input_height(cols, rows);
        let rail_h = self.rail_lines().len() as u16;
        (rows as i32 - 1 - rail_h as i32 - popup_h as i32 - input_h as i32 - 1).max(1) as u16
    }

    /// The bottom box: the composer, or the ask card that REPLACES it. Sized
    /// from `ask_card_height` over the same wrapped lines the card renders, so
    /// the frame reserved and the rows drawn are the same number.
    fn input_height(&self, cols: u16, rows: u16) -> u16 {
        if let Some(ask) = &self.ask {
            let lines = ask_prompt_lines(&ask.question, rows, cols);
            let options = ask.options.as_ref().map(|o| o.len()).unwrap_or(0);
            return ask_card_height(lines.len(), options) as u16;
        }
        let composer_rows = ((rows as usize) / 4).clamp(3, 8);
        composer_height(&self.draft, "", self.busy(), cols, composer_rows, 0) as u16
    }

    /// Rows the open tab may paint. The panel's own box takes two of the
    /// growing region for its border.
    fn panel_body_budget(&self) -> usize {
        let chat_h = self.chat_height(self.cols.max(20), self.rows.max(8), 0);
        panel_body_rows((chat_h as usize).saturating_sub(2))
    }

    /// The effective mode (spec §4): the panel outranks everything, and the
    /// overlay outranks the panel. Stored flags hold only chat/help.
    fn ui_mode(&self) -> UiMode {
        if self.help_open {
            UiMode::Help
        } else if self.job.is_some() {
            // The job view is opened FROM the rail and returns to it, so it
            // outranks it; the overlay still outranks both.
            UiMode::Job
        } else if self.rail_sel.is_some() {
            UiMode::Rail
        } else if self.panel.open() {
            UiMode::Panel
        } else if self.ask.is_some() {
            // A held ask() replaces the composer and owns the keyboard — the
            // turn is parked until it is answered.
            UiMode::Ask
        } else {
            UiMode::Chat
        }
    }

    /// The command a keypress means in the current context — the ONE place a
    /// key is interpreted for the surfaces this row owns.
    fn resolve(&self, k: &KeyEvent) -> Option<Command> {
        let (input, flags) = key_chord(k);
        let ctx = KeyContext {
            mode: self.ui_mode(),
            tab: self.panel.open().then(|| self.panel.tab()),
            empty_draft: self.draft.is_empty(),
            busy: self.busy(),
            quit_armed: self.quit_armed,
            // The popup owns ⏎/↑/↓/⇥ while it is open, and it is a chat-side
            // surface: naming it here keeps this lookup from claiming keys the
            // composer is about to answer.
            completing: self.trigger().is_some(),
            // ↓ from an empty composer enters the rail only when there IS one.
            rail_live: !self.units().is_empty(),
            // Inside the window Escape means the take-back, and it outranks the
            // stop — nobody takes a message back and still wants the answer.
            just_sent: self.just_sent(),
            ..Default::default()
        };
        lookup(&ctx, &crate::keys::chord_of(&input, flags))
    }

    /// A `/command` this client answers itself (a tab, the overlay).
    fn run_client_command(&mut self, command: Command) {
        match command {
            Command::HelpOpen => {
                self.help_open = true;
                self.help_off = 0;
            }
            Command::HelpClose => self.help_open = false,
            other => {
                let requests = self.panel.handle(other, None, self.panel_body_budget());
                self.serve(requests);
            }
        }
    }

    fn serve(&mut self, requests: Vec<HostRequest>) {
        for request in requests {
            match request {
                HostRequest::LoadSessions => self.transport.effect(Effect::LoadSessions),
                HostRequest::LoadChanges => self.transport.effect(Effect::LoadChanges),
                HostRequest::Open(id) => self.transport.effect(Effect::OpenSession(id)),
                HostRequest::Revert(paths) => self.transport.effect(Effect::Revert(paths)),
                HostRequest::LoadTheme => self.transport.effect(Effect::LoadTheme),
                HostRequest::SaveTheme(write) => self.transport.effect(Effect::SaveTheme(write)),
            }
        }
    }

    /// Keys for the overlay and the panel, ahead of the composer's own. The
    /// panel gets first refusal (spec §4) — while it is open, the keyboard is
    /// its own, and the composer says so.
    fn on_surface_key(&mut self, k: &KeyEvent) -> bool {
        let digit = match k.code {
            KeyCode::Char(c) if c.is_ascii_digit() => c.to_digit(10).map(|d| d as usize),
            _ => None,
        };
        let Some(command) = self.resolve(k) else {
            // The ask card is the ONE keyboard-owning surface that still takes
            // text: free text is always a possible answer, so an unbound key is
            // typed into the card rather than eaten.
            if self.ui_mode() == UiMode::Ask {
                return self.type_into_ask(k);
            }
            // In a surface that owns the keyboard, an unbound key is eaten
            // rather than typed into a composer nobody can see.
            return self.help_open || self.panel.open() || self.job.is_some()
                || self.rail_sel.is_some();
        };
        // `^c` is bound in EVERY mode and is the one way out of a wedged
        // terminal: no surface may swallow it, so it falls through to the
        // two-press quit below whatever is open.
        if matches!(command, Command::Quit | Command::QuitArm) {
            return false;
        }
        match self.ui_mode() {
            UiMode::Help => {
                let total = overlay_lines().len();
                let rows = self.rows.max(8) as usize;
                match command {
                    Command::HelpClose => self.help_open = false,
                    // The overlay scrolls like a page: ↑ moves toward the top.
                    Command::ScrollUp => self.help_off = self.help_off.saturating_sub(HELP_STEP),
                    Command::ScrollDown => {
                        self.help_off = clamp_help_offset(self.help_off + HELP_STEP, total, rows)
                    }
                    Command::ScrollPageUp => {
                        self.help_off = self.help_off.saturating_sub(self.page())
                    }
                    Command::ScrollPageDown => {
                        self.help_off = clamp_help_offset(self.help_off + self.page(), total, rows)
                    }
                    _ => {}
                }
                true
            }
            UiMode::Job => {
                self.on_job_command(command);
                true
            }
            UiMode::Rail => {
                self.on_rail_command(command, digit);
                true
            }
            UiMode::Ask => {
                self.on_ask_command(command, digit, k);
                true
            }
            UiMode::Panel => {
                // `^t` and the tab chords resolve in panel mode too, so the
                // panel closes itself and a jump lands without a round trip.
                let requests = self.panel.handle(command, digit, self.panel_body_budget());
                self.serve(requests);
                true
            }
            // From chat only the chords that OPEN a surface are claimed here;
            // everything else falls through to the composer.
            _ => match command {
                Command::HelpOpen => {
                    self.help_open = true;
                    self.help_off = 0;
                    true
                }
                Command::PanelToggle | Command::Tab(_) => {
                    let requests = self.panel.handle(command, None, self.panel_body_budget());
                    self.serve(requests);
                    true
                }
                // ↓ from an empty composer moves INTO the rail (guarded on
                // there being one), and it is reversible with esc.
                Command::RailEnter => {
                    self.rail_sel = Some(0);
                    true
                }
                // The take-back window's Escape. The keymap decided this
                // outranks the stop; this arm is only the gesture.
                Command::MessageUnsend => {
                    self.take_back();
                    true
                }
                _ => false,
            },
        }
    }

    // ---- the rail's keys ---------------------------------------------------

    fn on_rail_command(&mut self, command: Command, _digit: Option<usize>) {
        let units = self.units();
        let at = self.rail_sel.unwrap_or(0).min(units.len().saturating_sub(1));
        match command {
            Command::RailUp => {
                self.rail_armed = None;
                self.rail_sel = Some(at.saturating_sub(1));
            }
            Command::RailDown => {
                self.rail_armed = None;
                self.rail_sel = Some((at + 1).min(units.len().saturating_sub(1)));
            }
            Command::RailExit => {
                self.rail_sel = None;
                self.rail_armed = None;
            }
            Command::RailOpen => {
                let Some(unit) = units.get(at) else { return };
                match unit.kind {
                    // The whole point of the job view: what a shell printed is
                    // already in the server's memory, and reading it must not
                    // cost a turn.
                    LiveUnitKind::Shell => {
                        self.job = Some(JobView {
                            id: unit.id.clone(),
                            output: String::new(),
                            job: self.jobs.iter().find(|j| j.id == unit.id).cloned(),
                            error: None,
                            scroll: 0,
                            armed: false,
                        });
                        self.transport.effect(Effect::LoadJobOutput(unit.id.clone()));
                    }
                    LiveUnitKind::Subagent => {
                        self.transport.effect(Effect::OpenSession(unit.id.clone()))
                    }
                    LiveUnitKind::Workflow | LiveUnitKind::Schedule => {
                        self.notice =
                            Some("that surface is not wired into this client yet".to_string())
                    }
                }
            }
            Command::RailStop => {
                let Some(unit) = units.get(at) else { return };
                // Two presses, always: the first says what the second destroys.
                if self.rail_armed.as_deref() != Some(unit.id.as_str()) {
                    self.rail_armed = Some(unit.id.clone());
                    return;
                }
                self.rail_armed = None;
                match unit.kind {
                    LiveUnitKind::Shell => {
                        self.transport.effect(Effect::KillJob(unit.id.clone()))
                    }
                    LiveUnitKind::Subagent => {
                        self.transport.effect(Effect::StopSession(unit.id.clone()))
                    }
                    LiveUnitKind::Workflow | LiveUnitKind::Schedule => {
                        self.notice =
                            Some("stopping that is not wired into this client yet".to_string())
                    }
                }
            }
            // esc cancels the arm before it leaves the rail.
            Command::Cancel => self.rail_armed = None,
            _ => {}
        }
    }

    // ---- the job view's keys -----------------------------------------------

    fn on_job_command(&mut self, command: Command) {
        let page = self.page();
        let Some(view) = self.job.as_mut() else { return };
        match command {
            Command::JobClose => self.job = None,
            Command::ScrollUp => view.scroll += 1,
            Command::ScrollDown => view.scroll = view.scroll.saturating_sub(1),
            Command::ScrollPageUp => view.scroll += page,
            Command::ScrollPageDown => view.scroll = view.scroll.saturating_sub(page),
            Command::JobStop => {
                if !view.armed {
                    view.armed = true;
                    return;
                }
                view.armed = false;
                let id = view.id.clone();
                self.transport.effect(Effect::KillJob(id));
            }
            Command::Cancel => view.armed = false,
            _ => {}
        }
    }

    // ---- the ask card's keys -----------------------------------------------

    fn on_ask_command(&mut self, command: Command, digit: Option<usize>, k: &KeyEvent) {
        let Some(ask) = self.ask.clone() else { return };
        match command {
            Command::AskPick => {
                // A digit picks an option; the card numbers them from 1.
                let Some(d) = digit.filter(|d| *d > 0) else { return };
                let Some(option) =
                    ask.options.as_ref().and_then(|o| o.get(d - 1)).cloned()
                else {
                    return;
                };
                self.answer_ask(&ask, option);
            }
            Command::AskSend => {
                // An empty ⏎ is not an answer: the hold stays and the card says
                // so by simply not moving.
                if self.ask_typed.trim().is_empty() {
                    return;
                }
                let text = self.ask_typed.clone();
                self.answer_ask(&ask, text);
            }
            Command::AskDecline => {
                self.ask = None;
                self.ask_typed.clear();
                self.transport.effect(Effect::DeclineAsk {
                    session_id: ask.session_id.clone(),
                    id: ask.id.clone(),
                });
            }
            // Backspace is bound to nothing in Ask mode, so it arrives here
            // only if a future binding claims it; typing is `type_into_ask`.
            _ => {
                self.type_into_ask(k);
            }
        }
    }

    /// Free text into the card. Returns true always — the card owns the keys.
    fn type_into_ask(&mut self, k: &KeyEvent) -> bool {
        match k.code {
            KeyCode::Backspace => {
                self.ask_typed.pop();
            }
            KeyCode::Char(c)
                if !c.is_control()
                    && !k.modifiers.contains(KeyModifiers::ALT)
                    && !k.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.ask_typed.push(c)
            }
            _ => {}
        }
        true
    }

    fn answer_ask(&mut self, ask: &AskQuestion, answer: String) {
        self.ask = None;
        self.ask_typed.clear();
        self.transport.effect(Effect::AnswerAsk {
            session_id: ask.session_id.clone(),
            id: ask.id.clone(),
            answer,
        });
    }

    // ---- the take-back -----------------------------------------------------

    /// The gesture, decided as data by the ported rule and performed here.
    /// Nothing to take back does NOTHING — falling through to a stop would be
    /// an action the user did not ask for, and the next Escape is outside the
    /// window and stops it.
    fn take_back(&mut self) {
        match crate::forest::take_back_target(&[], &self.thread) {
            crate::forest::TakeBack::Sent { at_message_id, .. } => {
                self.transport.effect(Effect::Unsend(at_message_id))
            }
            // This client has no queue (nothing is held while busy), so a
            // queued take-back cannot arise; `None` is the honest no-op.
            crate::forest::TakeBack::Queued | crate::forest::TakeBack::None => {}
        }
    }

    fn on_key(&mut self, k: KeyEvent, now_ms: i64) {
        if k.kind == KeyEventKind::Release {
            return;
        }
        if self.on_surface_key(&k) {
            // Any chord other than ^c disarms the quit (App.tsx).
            self.quit_armed = false;
            return;
        }
        if self.on_completion_key(&k) {
            self.quit_armed = false;
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        // Any chord other than ^c disarms the quit (App.tsx).
        let is_ctrl_c = ctrl && matches!(k.code, KeyCode::Char('c'));
        if !is_ctrl_c {
            self.quit_armed = false;
        }
        match (k.code, ctrl) {
            (KeyCode::Char('c'), true) => {
                if self.quit_armed {
                    self.quit = true;
                } else {
                    self.quit_armed = true;
                    self.notice = Some(
                        "^c again to quit — subagents and workflows keep running".to_string(),
                    );
                }
            }
            (KeyCode::Esc, _) => self.on_escape(now_ms),
            (KeyCode::Enter, _) => {
                // shift/alt+enter (and ^j below) insert a newline; bare enter sends.
                if k.modifiers.contains(KeyModifiers::SHIFT)
                    || k.modifiers.contains(KeyModifiers::ALT)
                {
                    self.insert_char('\n');
                } else {
                    self.submit();
                }
            }
            (KeyCode::Char('j'), true) => self.insert_char('\n'),
            (KeyCode::Char('a'), true) => self.cursor_home(),
            (KeyCode::Char('e'), true) => self.cursor_end(),
            (KeyCode::Home, _) => self.cursor_home(),
            (KeyCode::End, _) => self.cursor_end(),
            (KeyCode::Backspace, _) => self.delete_back(),
            (KeyCode::Left, _) => self.cursor = self.cursor.saturating_sub(1),
            (KeyCode::Right, _) => {
                self.cursor = (self.cursor + 1).min(self.draft.chars().count())
            }
            (KeyCode::PageUp, _) => self.scroll_by(self.page() as isize),
            (KeyCode::PageDown, _) => self.scroll_by(-(self.page() as isize)),
            (KeyCode::Char(c), false) => {
                // stripCtl-lite: whole control chars never reach the draft;
                // meta chords are commands, never text (inkKey: Option = meta).
                if !c.is_control() && !k.modifiers.contains(KeyModifiers::ALT) {
                    self.insert_char(c);
                }
            }
            _ => {}
        }
    }

    /// Escape, ahead of everything else (App.tsx): a stop at a running turn is
    /// never delayed by the rewind — "esc esc still stops a running turn".
    fn on_escape(&mut self, now_ms: i64) {
        if self.busy() {
            // v1 note: the TS holds the AMBIGUOUS case (busy + non-empty draft)
            // for 600ms before interrupting; with the rewind unwired there is
            // no ambiguity yet, so the stop fires immediately on every path.
            self.transport.effect(Effect::Interrupt);
            self.last_esc_at = None;
            return;
        }
        if !self.draft.is_empty() {
            let double = self
                .last_esc_at
                .is_some_and(|at| now_ms - at < DOUBLE_ESC_MS);
            if double {
                self.clear_draft();
                self.last_esc_at = None;
            } else {
                self.last_esc_at = Some(now_ms);
            }
            return;
        }
        // cancel: reset scroll, dismiss notice.
        self.scroll_off = 0;
        self.notice = None;
        self.last_esc_at = Some(now_ms);
    }

    // ---- line editing (keys.ts subset; cursor is a char index) -------------

    fn byte_at(&self, char_idx: usize) -> usize {
        self.draft
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.draft.len())
    }

    fn insert_char(&mut self, c: char) {
        let at = self.byte_at(self.cursor);
        self.draft.insert(at, c);
        self.cursor += 1;
        // Typing re-opens a popup an earlier esc closed: esc dismisses THIS
        // token, not completion in general.
        self.dismissed = false;
        self.ensure_candidates();
    }

    fn delete_back(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let at = self.byte_at(self.cursor - 1);
        self.draft.remove(at);
        self.cursor -= 1;
        self.ensure_candidates();
    }

    /// Home/end are the LOGICAL line's (keys.ts).
    fn cursor_home(&mut self) {
        let chars: Vec<char> = self.draft.chars().collect();
        let mut i = self.cursor;
        while i > 0 && chars[i - 1] != '\n' {
            i -= 1;
        }
        self.cursor = i;
    }

    fn cursor_end(&mut self) {
        let chars: Vec<char> = self.draft.chars().collect();
        let mut i = self.cursor;
        while i < chars.len() && chars[i] != '\n' {
            i += 1;
        }
        self.cursor = i;
    }

    fn clear_draft(&mut self) {
        self.draft.clear();
        self.cursor = 0;
    }

    fn page(&self) -> usize {
        (self.rows as usize).saturating_sub(8).max(1)
    }

    // ---- submit -------------------------------------------------------------

    fn submit(&mut self) {
        let text = self.draft.clone();
        if text.is_empty() {
            return;
        }
        // The `!` shell and the `/` commands are store/keymap territory
        // (rows 1.35/1.36); until they land the sigils must not reach the
        // model — a `!echo hi` billed as a prompt is the exact TS bug the
        // sigil handling fixed. Absent capability is stated, never faked.
        if text.starts_with('!') {
            self.notice = Some("the ! shell is not wired into this client yet".to_string());
            return; // draft kept for editing
        }
        // SLASH DISPATCH RUNS AT SEND TIME, not only from the popup: the popup
        // opens as you type, so text that arrives faster than a render — a
        // paste, a fast typist, anything that delivers the line and its Return
        // in one read — never opened it, and Enter then sent `/model` to the
        // frontier model as an ordinary sentence. Measured: 19k tokens billed
        // and a conversation auto-titled "Model Architecture Discussion".
        //
        // A BARE `/word` IS A COMMAND ATTEMPT, NEVER PROSE. An unrecognised one
        // used to be sent: `/clear`, typed out of Claude Code habit, reached
        // haiku, which answered "Done. State cleared." and offered to revert the
        // workspace's modified files — a confirmation for something that never
        // happened. The draft is KEPT so the user can edit it.
        let skill_names: Vec<&str> = self.skills.iter().map(|(n, _)| n.as_str()).collect();
        if let Some((name, suggestion)) = unknown_command(&text, &skill_names) {
            self.notice = Some(format!(
                "there is no /{name}{} · type / for the list, or ? for every key",
                suggestion.map(|s| format!(" — did you mean /{s}?")).unwrap_or_default(),
            ));
            return; // draft kept
        }
        if let Some((command, arg)) = slash_invocation(&text) {
            self.clear_draft();
            self.transport.effect(Effect::Run(command, arg));
            return;
        }
        self.clear_draft();
        self.scroll_off = 0;
        // The take-back window opens here, and it SAYS so: three seconds that
        // nothing announces is a gesture only the keymap knows about.
        self.last_send_at = Some(self.now_ms);
        self.notice = Some(TAKE_BACK_HINT.to_string());
        // Optimistic local echo; the snapshot/SSE merge reconciles by id later.
        self.sent_seq += 1;
        self.thread.push(Message {
            id: format!("local-{}", self.sent_seq),
            session_id: String::new(),
            role: Role::User,
            parts: vec![Part::Text { text: text.clone() }],
            pending: false,
            created_at: self.now_ms,
        });
        self.transport.effect(Effect::Send(text));
    }

    // ---- SSE reduce ---------------------------------------------------------

    /// Exhaustive over the closed 16-name set — NO default arm: a new event
    /// type must be a compile error (schema/events.rs contract).
    fn reduce_event(&mut self, event: BoughEvent) {
        // The stream is UN-scoped (the TS store subscribes to everything and
        // reduces per session); this v1 screen shows one conversation, so
        // another session's events must not stream into its thread. Un-scoped
        // events pass regardless.
        if let Some(sid) = &event.session_id {
            if self.session_id.as_ref() != Some(sid) {
                return;
            }
        }
        match event.r#type {
            EventType::SessionCreated => {}
            EventType::SessionUpdated => {}
            EventType::SessionActivity => {
                if let Ok(d) = serde_json::from_value::<SessionActivityData>(event.data) {
                    self.activity = d.activity;
                }
            }
            EventType::MessageStarted => {
                let Ok(msg) = serde_json::from_value::<Message>(event.data) else {
                    return; // malformed frames are skipped, never fatal
                };
                // A pending message IS the turn starting (there is no
                // turn.started event); the clock is the event's ts, never a
                // wall clock read in the reducer.
                if msg.pending {
                    self.turn = Some(TurnClock { started_at: event.ts, ended: false });
                }
                // The server's copy of a message we sent supersedes the
                // optimistic local echo (the TS store reconciles by id via the
                // snapshot merge; v1 matches the echo by text).
                if msg.role == Role::User {
                    let text = first_text(&msg);
                    if let Some(pos) = self.thread.iter().position(|m| {
                        m.id.starts_with("local-") && first_text(m) == text
                    }) {
                        self.thread.remove(pos);
                    }
                }
                if let Some(existing) = self.thread.iter_mut().find(|m| m.id == msg.id) {
                    *existing = msg;
                } else {
                    self.thread.push(msg);
                }
            }
            EventType::MessageDelta => {
                if let Ok(d) = serde_json::from_value::<MessageDeltaData>(event.data) {
                    self.streaming.entry(d.message_id).or_default().push_str(&d.delta);
                }
            }
            EventType::MessagePart => {
                let Ok(d) = serde_json::from_value::<MessagePartData>(event.data) else {
                    return;
                };
                // A finalized text part supersedes the streamed copy of it.
                if matches!(d.part, Part::Text { .. }) {
                    self.streaming.remove(&d.message_id);
                }
                if let Some(msg) = self.thread.iter_mut().find(|m| m.id == d.message_id) {
                    msg.parts.push(d.part);
                }
            }
            EventType::MessageFinished => {
                if let Ok(d) = serde_json::from_value::<MessageFinishedData>(event.data) {
                    self.streaming.remove(&d.message_id);
                    if let Some(msg) = self.thread.iter_mut().find(|m| m.id == d.message_id) {
                        msg.pending = false;
                    }
                }
            }
            EventType::MessageRetry => {
                // The re-stream is a competing copy, not a continuation.
                if let Ok(d) = serde_json::from_value::<MessageRetryData>(event.data) {
                    self.streaming.remove(&d.message_id);
                }
            }
            EventType::ToolLog => {} // live tool folds land with lines.rs (row 1.37)
            EventType::TurnFinished => {
                if serde_json::from_value::<TurnFinishedData>(event.data).is_ok() {
                    if let Some(turn) = &mut self.turn {
                        turn.ended = true;
                    }
                    for msg in &mut self.thread {
                        msg.pending = false;
                    }
                    self.activity = None;
                }
            }
            // A hold is raised AND settled on this event: the payload carries
            // the status, so an answered/declined/interrupted hold takes the
            // card down rather than leaving a question nobody can still answer.
            EventType::AskQuestion => {
                let Ok(q) = serde_json::from_value::<AskQuestion>(event.data) else {
                    return;
                };
                if q.status == AskQuestionStatus::Pending {
                    self.ask_typed.clear();
                    self.ask = Some(q);
                } else if self.ask.as_ref().is_some_and(|open| open.id == q.id) {
                    self.ask = None;
                    self.ask_typed.clear();
                }
            }
            // The rail's feed is the LISTING, not the event: a spawn/exit says
            // "re-read", so one shape (`GET /jobs`) decides what is running and
            // an event that arrives out of order cannot invent a row.
            EventType::JobSpawned | EventType::JobExited => {
                if self.session_id.is_some() {
                    self.transport.effect(Effect::PollJobs);
                }
            }
            EventType::WorkflowUpdated => {}
            EventType::WorkflowAgent => {}
            EventType::WorkflowLog => {}
        }
    }

    // ---- transcript (v1 miniature of lines.rs buildLines) -------------------

    fn transcript_lines(&self) -> Vec<String> {
        let width = self.cols as usize;
        let body_w = width.saturating_sub(2).max(20);
        let mut out: Vec<String> = Vec::new();
        for msg in &self.thread {
            out.push(String::new());
            out.push(
                match msg.role {
                    Role::User => "you",
                    Role::Supervisor => "bough",
                    Role::System => "system",
                }
                .to_string(),
            );
            for part in &msg.parts {
                match part {
                    Part::Text { text } => {
                        for line in text.split('\n') {
                            for row in hard_wrap(line, body_w) {
                                out.push(format!("  {row}"));
                            }
                        }
                    }
                    Part::Reasoning { text, .. } => {
                        let n = text.split('\n').count();
                        out.push(format!(
                            "  thinking ({n} line{})",
                            if n == 1 { "" } else { "s" }
                        ));
                    }
                    Part::ToolCall { name, .. } => out.push(format!("  ⚙ {name}")),
                    Part::ToolResult { .. } => {} // folded by default
                    Part::Image { name, .. } => out.push(format!("  [image: {name}]")),
                    Part::Ask { question, .. } => out.push(format!("  ? {question}")),
                    Part::Workflow { name, .. } => out.push(format!("  ⧉ {name}")),
                }
            }
            if msg.pending {
                if let Some(streamed) = self.streaming.get(&msg.id) {
                    for line in format!("{streamed}▌").split('\n') {
                        for row in hard_wrap(line, body_w) {
                            out.push(format!("  {row}"));
                        }
                    }
                }
            }
        }
        out
    }

    // ---- draw ---------------------------------------------------------------

    /// The panel, in the growing region. Its own box takes two rows of the
    /// budget for the border, and the tab bodies are clamped to what is left.
    fn draw_panel(&self, area: Rect, buf: &mut Buffer) {
        let rows = self.panel.rows();
        let body = match self.panel.tab() {
            crate::keys::PanelTab::Tree => {
                PanelBody::Tree(crate::components::panel::tree::TreeProps {
                    rows: &rows,
                    selected: self.panel.sel,
                    height: panel_body_rows((area.height as usize).saturating_sub(2)),
                    workspace: self.options.workspace.as_deref(),
                    cols: Some((area.width as usize).saturating_sub(4).max(20)),
                    message: self.panel.message.as_deref(),
                    ..Default::default()
                })
            }
            crate::keys::PanelTab::Changes => {
                PanelBody::Changes(crate::components::panel::changes::ChangesProps {
                    set: self.panel.changes.as_ref(),
                    items: &self.panel.items,
                    selected: self.panel.sel,
                    scroll: self.panel.diff_scroll,
                    rows: panel_body_rows((area.height as usize).saturating_sub(2)),
                    focused: self.panel.diff_focused,
                    message: self.panel.message.as_deref(),
                    pending: self.panel.pending.as_ref(),
                    // With no conversation open there is no checkout, and the
                    // non-git sentence would be a claim about a directory that
                    // does not exist.
                    hint: self.session_id.as_ref().map(|_| {
                        crate::components::panel::changes::NOT_A_REPO_HINT
                    }),
                })
            }
            crate::keys::PanelTab::Theme => PanelBody::Theme(self.panel.theme.as_ref()),
            // The remaining tabs land in row 3.20; an absent surface says so
            // rather than painting an empty box.
            _ => PanelBody::Text("nothing to show here yet"),
        };
        render_panel(self.panel.tab(), &body, area, buf);
    }

    pub fn draw(&self, area: Rect, buf: &mut Buffer) {
        let cols = area.width.max(20);
        let rows = area.height.max(8);
        // The overlay is the one surface that displaces everything, header
        // and composer included.
        if self.help_open {
            render_help(rows as usize, self.help_off, area, buf);
            return;
        }
        let lines = self.transcript_lines();
        let busy = self.busy();
        // App.tsx: composerRows = min(8, max(3, rows/4)).
        let composer_rows = ((rows as usize) / 4).clamp(3, 8);
        let input_h = self.input_height(cols, rows);
        // The popup is drawn ABOVE the box and takes its rows from the
        // transcript, exactly as `completionPopupHeight` mirrors `Composer`'s
        // render (App.tsx). A trigger that matched nothing still draws — a
        // picker that vanishes reads as broken rather than as empty.
        let trigger = self.trigger();
        let ranked = self.completion();
        let more = ranked.total.saturating_sub(ranked.items.len());
        let popup_h = match &trigger {
            Some(_) => completion_popup_height(ranked.items.len(), more) as u16,
            None => 0,
        };
        // ONE derivation, shared with the key handler (`chat_height`).
        let chat_h = self.chat_height(cols, rows, popup_h);

        // Header: the conversation's title and nothing else (plus the
        // disconnect suffix, which is a fact about right now).
        let title = self
            .options
            .workspace
            .as_deref()
            .map(|w| w.trim_end_matches('/').rsplit('/').next().unwrap_or(w).to_string())
            .unwrap_or_else(|| "new conversation".to_string());
        let mut header = vec![Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        )];
        if !self.connected {
            header.push(Span::styled("  · disconnected", Style::default().fg(WARN)));
        }
        buf.set_line(area.x, area.y, &Line::from(header), cols);

        let elapsed_ms = match &self.turn {
            Some(t) if !t.ended => (self.now_ms - t.started_at).max(0),
            _ => 0,
        };
        let growing = Rect { x: area.x, y: area.y + 1, width: cols, height: chat_h };
        // The growing region is the transcript OR the panel — the panel
        // DISPLACES it rather than floating over it, which is what makes
        // "there is exactly one place that is not the chat" true on screen.
        if let Some(view) = &self.job {
            // The open job takes the growing region: it is a reading surface,
            // and it returns to the rail it was opened from.
            let sub = job_sub_lines(
                view.job.as_ref(),
                &view.id,
                cols as usize,
                chat_h as usize,
            );
            render_job_output(
                &JobOutputProps {
                    id: &view.id,
                    job: view.job.as_ref(),
                    output: &view.output,
                    scroll: view
                        .scroll
                        .min(view.output.lines().count().saturating_sub(
                            job_body_rows(chat_h as usize, sub.len()),
                        )),
                    width: cols as usize,
                    height: chat_h as usize,
                    now: self.now_ms,
                    error: view.error.as_deref(),
                    armed: view.armed,
                },
                growing,
                buf,
            );
        } else if self.panel.open() {
            self.draw_panel(growing, buf);
        } else {
            render_chat(
                &ChatProps {
                    lines: &lines,
                    width: cols,
                    height: chat_h,
                    scroll_off: self.scroll_off,
                    activity: self.activity.as_deref(),
                    busy,
                    elapsed_ms,
                    turn_tokens: None, // usage polling lands with the store (row 1.35)
                    tick: self.tick,
                    queued: &[],
                    notice: self.notice.as_deref(),
                    placeholder: CHAT_PLACEHOLDER,
                },
                growing,
                buf,
            );
        }

        if let Some(trigger) = &trigger {
            render_completion_popup(
                &CompletionPopupProps {
                    kind: trigger.kind,
                    items: &ranked.items,
                    sel: Some(self.sel_at(ranked.items.len())),
                    more,
                },
                Rect { x: area.x, y: area.y + 1 + chat_h, width: cols, height: popup_h },
                buf,
            );
        }

        // The rail sits between the transcript and the composer: pinned, one
        // screen row per unit, and NOTHING at all when nothing is running.
        let rail = self.rail_lines();
        let rail_h = rail.len() as u16;
        if rail_h > 0 {
            render_rail(
                &self.units(),
                self.rail_sel,
                self.rail_armed.as_deref(),
                Rect { x: area.x, y: area.y + 1 + chat_h + popup_h, width: cols, height: rail_h },
                buf,
            );
        }

        let input_at = Rect {
            x: area.x,
            y: area.y + 1 + chat_h + popup_h + rail_h,
            width: cols,
            height: input_h,
        };
        // A held ask() REPLACES the composer — the turn is parked on it, and a
        // composer beside the card would invite typing that goes nowhere.
        if let Some(ask) = &self.ask {
            let lines = ask_prompt_lines(&ask.question, rows, cols);
            let options = ask.options.clone().unwrap_or_default();
            render_ask_card(
                &AskCardProps { lines: &lines, options: &options, typed: &self.ask_typed },
                input_at,
                buf,
            );
        } else {
            render_composer(
                &ComposerProps {
                    input: &self.draft,
                    cursor: self.cursor,
                    busy,
                    width: cols,
                    max_rows: composer_rows,
                    ghost: "", // ghost absent by contract in v1
                    attachments: &[],
                    // While the panel is open the keyboard is ITS own, and the box
                    // says so: a block cursor is the strongest claim a terminal UI
                    // can make about where typing goes.
                    keyboard_owner: self.panel.open().then(|| panel_owner(self.panel.tab())),
                },
                input_at,
                buf,
            );
        }

        render_status(
            &ChatMeter {
                workspace: self.options.workspace.clone(),
                help: true,
                ..Default::default()
            },
            Rect {
                x: area.x,
                y: (area.y + 1 + chat_h + popup_h + rail_h + input_h).min(area.y + rows - 1),
                width: cols,
                height: 1,
            },
            buf,
        );
    }
}

/// The production copy path: OSC 52 to stdout, which reaches the LOCAL
/// clipboard over ssh and tmux (term.rs owns the sequence and its cap).
fn osc52_copy(text: &str) {
    crate::term::create_term(crate::term::TermOptions {
        // OSC 52 is capability-independent (and never tmux-wrapped: tmux
        // translates it itself), but a Term needs its caps to exist.
        caps: crate::term::term_caps(&std::env::vars().collect()),
        write: std::rc::Rc::new(|seq: &str| {
            use std::io::Write;
            let mut out = std::io::stdout();
            let _ = out.write_all(seq.as_bytes());
            let _ = out.flush();
        }),
        rename_tmux_window: None,
        rename_zellij_tab: None,
        timers: std::rc::Rc::new(crate::term::NoopTimers::default()),
    })
    .osc52_copy(text);
}

/// The production link path: the platform opener, detached, failures ignored —
/// there is nothing useful to say to a user whose desktop has no handler.
fn open_url(url: &str) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = std::process::Command::new(opener)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// The change set for "no conversation is open". `available: false` with its
/// own reason, because there is no checkout to review — not an empty diff,
/// which would claim the session changed nothing.
fn no_session_changes() -> crate::store::state::SessionChangeSet {
    crate::store::state::SessionChangeSet {
        available: false,
        reason: Some("no conversation is open — open one to review its changes".to_string()),
        base: None,
        files: Vec::new(),
        workspace: None,
    }
}

/// What a revert actually did, per path. A path the server SKIPPED and one
/// that FAILED are different outcomes, and this row says which.
fn revert_outcome(outcome: &crate::api::RevertOutcome) -> String {
    let mut parts: Vec<String> = vec![if outcome.reverted.is_empty() {
        "nothing was reverted".to_string()
    } else {
        format!("reverted {}", outcome.reverted.join(", "))
    }];
    if !outcome.skipped.is_empty() {
        parts.push(format!("not in this change set: {}", outcome.skipped.join(", ")));
    }
    for f in &outcome.failed {
        parts.push(format!("failed {}: {}", f.path, f.error));
    }
    parts.join(" · ")
}

/// The surface name the composer prints while the panel has the keyboard.
fn panel_owner(tab: crate::keys::PanelTab) -> &'static str {
    crate::keys::TABS
        .iter()
        .find(|t| t.id == tab)
        .map(|t| t.title)
        .unwrap_or("the panel")
}

/// One crossterm key event as `keys.rs` reads it: the raw text plus the flag
/// set. Modified keys take their NAME; unmodified printable chars take the
/// character, which is what preserves capitals (`X` is its own binding).
fn key_chord(k: &KeyEvent) -> (String, KeyFlags) {
    let m = k.modifiers;
    let mut flags = KeyFlags {
        ctrl: m.contains(KeyModifiers::CONTROL),
        shift: m.contains(KeyModifiers::SHIFT),
        // macOS Option arrives as ALT and is meta everywhere in the keymap.
        meta: m.contains(KeyModifiers::ALT),
        super_: m.contains(KeyModifiers::SUPER),
        ..Default::default()
    };
    let mut input = String::new();
    match k.code {
        KeyCode::Up => flags.up_arrow = true,
        KeyCode::Down => flags.down_arrow = true,
        KeyCode::Left => flags.left_arrow = true,
        KeyCode::Right => flags.right_arrow = true,
        KeyCode::PageUp => flags.page_up = true,
        KeyCode::PageDown => flags.page_down = true,
        KeyCode::Home => flags.home = true,
        KeyCode::End => flags.end = true,
        KeyCode::Enter => flags.r#return = true,
        // `escape` clears the meta flag: ESC ESC arrives as ONE event flagged
        // meta, and a meta+esc chord is bound to nothing.
        KeyCode::Esc => {
            flags.escape = true;
            flags.meta = false;
        }
        KeyCode::Tab => flags.tab = true,
        KeyCode::BackTab => {
            flags.tab = true;
            flags.shift = true;
        }
        KeyCode::Backspace | KeyCode::Delete => flags.backspace = true,
        KeyCode::Char(c) => input.push(c),
        _ => {}
    }
    (input, flags)
}

fn first_text(msg: &Message) -> Option<&str> {
    msg.parts.iter().find_map(|p| match p {
        Part::Text { text } => Some(text.as_str()),
        _ => None,
    })
}

fn hard_wrap(line: &str, width: usize) -> Vec<String> {
    let w = width.max(1);
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars.chunks(w).map(|c| c.iter().collect()).collect()
}

// ---- the live loop ----------------------------------------------------------

/// Run the real terminal loop. All input tasks post over one mpsc; the reducer
/// and the draw stay on this task. `events` is the SSE action feed — the
/// composition root supplies it once `events.rs` (row 1.33) lands; an empty
/// feed renders the honest disconnected state.
pub async fn run_loop<T: Transport>(
    options: TuiOptions,
    transport: T,
    mut events: tokio::sync::mpsc::UnboundedReceiver<Action>,
) -> std::io::Result<()> {
    use crossterm::event::{DisableMouseCapture, EnableMouseCapture, EventStream};
    use futures::StreamExt;

    let mut terminal = ratatui::init();
    // Wheel scroll is the one mouse gesture wave 1 ships.
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    let size = terminal.size()?;
    let mut app = App::new(options, transport, size.width, size.height);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Action>();

    // crossterm input task.
    let term_tx = tx.clone();
    let input_task = tokio::spawn(async move {
        let mut stream = EventStream::new();
        while let Some(Ok(ev)) = stream.next().await {
            if term_tx.send(Action::Term(ev)).is_err() {
                break;
            }
        }
    });

    // Spinner/elapsed timer task.
    let tick_tx = tx.clone();
    let tick_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(
            crate::components::SPINNER_MS,
        ));
        loop {
            interval.tick().await;
            if tick_tx.send(Action::Tick).is_err() {
                break;
            }
        }
    });

    let result: std::io::Result<()> = loop {
        let action = tokio::select! {
            a = rx.recv() => a,
            e = events.recv() => e,
        };
        let Some(action) = action else { break Ok(()) };
        let is_tick = matches!(action, Action::Tick);
        let now = now_ms();
        app.apply(action, now);
        if app.quit {
            break Ok(());
        }
        // An idle tick changes nothing on screen — repainting on it would put
        // an 8fps write loop under every idle terminal (TS: no timer at all
        // when nothing is live). `busy()` alone was too narrow: the jobs poll
        // rides this timer, so a shell that outlives its turn could never
        // repaint the rail it belongs on.
        if is_tick && !app.animating() {
            continue;
        }
        if let Err(e) = terminal.draw(|f| {
            let area = f.area();
            app.draw(area, f.buffer_mut());
        }) {
            break Err(e);
        }
    };

    input_task.abort();
    tick_task.abort();
    // The terminal is restored on every exit path (main.tsx contract; the
    // panic-hook half is term.rs, row 1.38).
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---- the live composition (api.rs + events.rs wiring) -----------------------

/// The production [`Transport`]: every effect becomes a spawned REST call
/// whose outcome (a session id, a failure sentence) posts BACK over the same
/// mpsc the reducer drains — the loop task never awaits I/O.
struct LiveTransport {
    api: crate::api::Api,
    workspace: Option<String>,
    /// The session the first send creates; later effects reuse it.
    session: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    tx: tokio::sync::mpsc::UnboundedSender<Action>,
}

impl Transport for LiveTransport {
    fn effect(&mut self, effect: Effect) {
        let api = self.api.clone();
        let tx = self.tx.clone();
        let session = self.session.clone();
        let workspace = self.workspace.clone();
        match effect {
            Effect::Send(text) => {
                tokio::spawn(async move {
                    let known = session.lock().expect("session lock").clone();
                    let sid = match known {
                        Some(sid) => sid,
                        None => {
                            // First send creates the conversation on the default
                            // workspace (main.tsx / App.tsx submit contract). No
                            // title: the cheap tier names it server-side.
                            let body = bough_core::schema::requests::CreateSessionBody {
                                workspace,
                                ..Default::default()
                            };
                            match api.create_session(&body).await {
                                Ok(s) => {
                                    *session.lock().expect("session lock") = Some(s.id.clone());
                                    let _ = tx.send(Action::SessionOpened(s.id.clone()));
                                    s.id
                                }
                                Err(e) => {
                                    let _ = tx.send(Action::Notice(e.to_string()));
                                    return;
                                }
                            }
                        }
                    };
                    let body = bough_core::schema::requests::PostMessageBody { text, images: None };
                    if let Err(e) = api.post_message(&sid, &body).await {
                        let _ = tx.send(Action::Notice(e.to_string()));
                    }
                });
            }
            // Every candidate fetch is silent on failure: no repo, no
            // candidates — the popup simply stays empty, which is the same
            // experience as a query that matches nothing and is not worth a
            // modal (App.tsx's `.catch(() => {})`).
            Effect::LoadFiles => {
                tokio::spawn(async move {
                    let known = session.lock().expect("session lock").clone();
                    let listed = match (known, workspace) {
                        (Some(sid), _) => api.list_files(&sid).await.ok(),
                        // A conversation that has not run a turn has no session
                        // id — and that is the screen where someone first types
                        // `@`. Fall back to the workspace it WOULD start in.
                        (None, Some(dir)) => api.list_files_in(&dir).await.ok(),
                        (None, None) => None,
                    };
                    if let Some(list) = listed {
                        let _ = tx.send(Action::Files(list.files));
                    }
                });
            }
            Effect::LoadDirEntries(prefix) => {
                tokio::spawn(async move {
                    // A half-typed path is the middle of typing, not an error.
                    if let Ok(list) = api.list_dir_entries(&prefix, workspace.as_deref()).await {
                        let _ = tx.send(Action::DirEntries { prefix, entries: list.entries });
                    }
                });
            }
            Effect::LoadSkills => {
                tokio::spawn(async move {
                    // No skills, no `/` rows — never a modal.
                    if let Ok(list) = api.list_skills().await {
                        let _ = tx.send(Action::Skills(
                            list.skills.into_iter().map(|s| (s.name, s.description)).collect(),
                        ));
                    }
                });
            }
            Effect::Run(command, _arg) => {
                // The surfaces this client owns answer themselves, back over
                // the same mpsc; the rest are honest about what they cannot do
                // — and, crucially, the command still never reaches the model.
                if tab_for_command(command).is_some() || command == Command::HelpOpen {
                    let _ = self.tx.send(Action::Run(command));
                    return;
                }
                let name = SLASH_COMMANDS
                    .iter()
                    .find(|c| c.command == command)
                    .map(|c| c.name)
                    .unwrap_or("that command");
                let _ = self.tx.send(Action::Notice(format!(
                    "/{name} is not wired into this client yet"
                )));
            }
            Effect::LoadSessions => {
                tokio::spawn(async move {
                    // A tree with no rows is a tree, not an error: the panel
                    // says "no conversations yet" and stays usable.
                    if let Ok(rows) = api.list_sessions(None).await {
                        let _ = tx.send(Action::Sessions(rows));
                    }
                });
            }
            Effect::LoadChanges => {
                tokio::spawn(async move {
                    let known = session.lock().expect("session lock").clone();
                    let Some(sid) = known else {
                        // No conversation, no checkout — and the tab must not
                        // print the non-git sentence about a directory that
                        // does not exist.
                        let _ = tx.send(Action::Changes(Some(no_session_changes())));
                        return;
                    };
                    match api.changes(&sid).await {
                        Ok(set) => {
                            let _ = tx.send(Action::Changes(Some(set)));
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                            let _ = tx.send(Action::Changes(None));
                        }
                    }
                });
            }
            Effect::LoadTheme => {
                tokio::spawn(async move {
                    // A theme the server cannot answer for is not a reason to
                    // refuse the picker: `None` opens it on the built-in
                    // palette, and the legend says "current: Default".
                    let _ = tx.send(Action::Theme(api.get_theme().await.ok()));
                });
            }
            Effect::SaveTheme(write) => {
                tokio::spawn(async move {
                    // Write-behind: the screen is already painted, so a failed
                    // save is said out loud and never unpaints it.
                    if let Err(e) = api.write_theme(&write).await {
                        let _ = tx.send(Action::Notice(e.to_string()));
                    }
                });
            }
            Effect::OpenSession(id) => {
                tokio::spawn(async move {
                    // Fetch FIRST, then switch: the reverse order shows an
                    // empty transcript for one frame and reads as a session
                    // that lost its history.
                    match api.get_session(&id).await {
                        Ok(snapshot) => {
                            *session.lock().expect("session lock") = Some(id.clone());
                            let _ = tx.send(Action::SessionOpened(id));
                            let _ = tx.send(Action::Thread(snapshot.thread));
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                });
            }
            Effect::Revert(paths) => {
                tokio::spawn(async move {
                    let known = session.lock().expect("session lock").clone();
                    let Some(sid) = known else { return };
                    match api.revert_changes(&sid, paths.as_deref()).await {
                        // The outcome line is said out loud AND the change set
                        // is re-read: the list it was reverted from is stale
                        // the moment the files go back.
                        Ok(outcome) => {
                            let _ = tx.send(Action::Notice(revert_outcome(&outcome)));
                            if let Ok(set) = api.changes(&sid).await {
                                let _ = tx.send(Action::Changes(Some(set)));
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                });
            }
            Effect::PollJobs => {
                tokio::spawn(async move {
                    let Some(sid) = session.lock().expect("session lock").clone() else { return };
                    // A poll that fails is a beat with no news, never a modal:
                    // the rail keeps the rows it had and the next tick retries.
                    if let Ok(list) = api.list_jobs(&sid).await {
                        let _ = tx
                            .send(Action::Jobs(list.jobs.into_iter().map(|r| r.job).collect()));
                    }
                });
            }
            Effect::LoadJobOutput(job_id) => {
                tokio::spawn(async move {
                    let Some(sid) = session.lock().expect("session lock").clone() else { return };
                    match api.job_output(&sid, &job_id).await {
                        Ok(out) => {
                            let _ = tx.send(Action::JobOutput {
                                id: job_id,
                                output: out.output,
                                job: Some(out.job),
                                error: None,
                            });
                        }
                        // The view stays open and says WHY there is no buffer.
                        Err(e) => {
                            let _ = tx.send(Action::JobOutput {
                                id: job_id,
                                output: String::new(),
                                job: None,
                                error: Some(e.to_string()),
                            });
                        }
                    }
                });
            }
            Effect::KillJob(job_id) => {
                tokio::spawn(async move {
                    let Some(sid) = session.lock().expect("session lock").clone() else { return };
                    match api.kill_job(&sid, &job_id).await {
                        // The server's own sentence, and then a re-read: the
                        // rail row it was killed from is stale immediately.
                        Ok(ack) => {
                            let _ = tx.send(Action::Notice(ack.message));
                            if let Ok(list) = api.list_jobs(&sid).await {
                                let _ = tx.send(Action::Jobs(
                                    list.jobs.into_iter().map(|r| r.job).collect(),
                                ));
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                });
            }
            Effect::StopSession(id) => {
                tokio::spawn(async move {
                    // Addressed to the DELEGATE's own session, not this screen's.
                    if let Err(e) = api.interrupt(&id).await {
                        let _ = tx.send(Action::Notice(e.to_string()));
                    }
                });
            }
            Effect::LoadQuestions => {
                tokio::spawn(async move {
                    let Some(sid) = session.lock().expect("session lock").clone() else { return };
                    if let Ok(asks) = api.list_questions(Some(&sid)).await {
                        let _ = tx.send(Action::Asks(asks));
                    }
                });
            }
            Effect::AnswerAsk { session_id, id, answer } => {
                tokio::spawn(async move {
                    // The hold belongs to the session that RAISED it — a
                    // delegate's question is answered on the delegate.
                    if let Err(e) = api.answer_question(&session_id, &id, &answer).await {
                        let _ = tx.send(Action::Notice(e.to_string()));
                    }
                });
            }
            Effect::DeclineAsk { session_id, id } => {
                tokio::spawn(async move {
                    if let Err(e) = api.decline_question(&session_id, &id).await {
                        let _ = tx.send(Action::Notice(e.to_string()));
                    }
                });
            }
            Effect::Unsend(at_message_id) => {
                tokio::spawn(async move {
                    let Some(sid) = session.lock().expect("session lock").clone() else { return };
                    match api.unsend(&sid, &at_message_id).await {
                        Ok(result) => {
                            let _ = tx.send(Action::TookBack(result.text));
                            // The thread is now shorter: re-read it, once,
                            // authoritatively rather than patching locally.
                            if let Ok(snapshot) = api.get_session(&sid).await {
                                let _ = tx.send(Action::Thread(snapshot.thread));
                            }
                        }
                        // A refusal is an answer: the server has said why.
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                });
            }
            Effect::Interrupt => {
                tokio::spawn(async move {
                    let known = session.lock().expect("session lock").clone();
                    if let Some(sid) = known {
                        // Always resolves for an existing session; a turn that
                        // had already ended answers `{interrupted:false}` and
                        // needs no row.
                        if let Err(e) = api.interrupt(&sid).await {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                });
            }
        }
    }
}

/// Preflight, connect the un-scoped SSE stream, run the loop, tear down.
/// The error string is already the user-facing sentence (`bough tui: …`),
/// printed by the bin with exit 2 (main.tsx::preflight contract).
pub async fn run_live(options: TuiOptions) -> Result<(), String> {
    let api = crate::api::Api::new(crate::api::ApiOptions { base: None, fetch_fn: None });
    if let Err(e) = api.preflight().await {
        return Err(format!("bough tui: {e}"));
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Action>();
    let stream = crate::events::connect_events(crate::events::EventStreamOptions {
        url: None,
        base: Some(api.base().to_string()),
        session_id: None, // un-scoped: the reducer filters per session
        on_event: {
            let tx = tx.clone();
            Box::new(move |event| {
                let _ = tx.send(Action::Event(event));
            })
        },
        on_open: {
            let tx = tx.clone();
            Some(Box::new(move |_| {
                let _ = tx.send(Action::Connected(true));
            }))
        },
        on_close: {
            let tx = tx.clone();
            Some(Box::new(move |_| {
                let _ = tx.send(Action::Connected(false));
            }))
        },
        on_bad_frame: None, // malformed frames are skipped, never fatal
        retry_ms: None,
        fetch_fn: None,
    });

    let transport = LiveTransport {
        api,
        workspace: options.workspace.clone(),
        session: Default::default(),
        tx,
    };
    let result = run_loop(options, transport, rx).await;
    stream.close();
    result.map_err(|e| format!("bough tui: {e}"))
}

// ---- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use serde_json::json;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// The scripted store: records effects, feeds back nothing on its own.
    fn scripted() -> (Rc<RefCell<Vec<Effect>>>, impl FnMut(Effect)) {
        let effects: Rc<RefCell<Vec<Effect>>> = Rc::default();
        let sink = effects.clone();
        (effects, move |e| sink.borrow_mut().push(e))
    }

    /// The effects that ACT — everything but the candidate fetches, which are
    /// bookkeeping behind the popup and say nothing about what was sent.
    fn sends(effects: &Rc<RefCell<Vec<Effect>>>) -> Vec<Effect> {
        effects
            .borrow()
            .iter()
            .filter(|e| {
                !matches!(
                    e,
                    Effect::LoadFiles
                        | Effect::LoadSkills
                        | Effect::LoadDirEntries(_)
                        // The rail's feed is bookkeeping too: it says nothing
                        // about what the user asked this client to do.
                        | Effect::PollJobs
                        | Effect::LoadQuestions
                )
            })
            .cloned()
            .collect()
    }

    fn open_s1<T: Transport>(app: &mut App<T>) {
        app.apply(Action::SessionOpened("s1".into()), 0);
    }

    fn key(code: KeyCode) -> Action {
        Action::Term(TermEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn ctrl(c: char) -> Action {
        Action::Term(TermEvent::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::CONTROL,
        )))
    }

    fn type_text<T: Transport>(app: &mut App<T>, text: &str, now: i64) {
        for c in text.chars() {
            app.apply(key(KeyCode::Char(c)), now);
        }
    }

    fn event(t: EventType, ts: i64, data: serde_json::Value) -> Action {
        Action::Event(BoughEvent {
            r#type: t,
            session_id: Some("s1".into()),
            seq: 1,
            ts,
            data,
        })
    }

    fn frame_of<T: Transport>(app: &App<T>, cols: u16, rows: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        term.draw(|f| {
            let area = f.area();
            app.draw(area, f.buffer_mut());
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// PORT_PLAN 1.39 gate, TestBackend half: type → stream → interrupt →
    /// scroll, over a scripted store.
    #[test]
    fn type_stream_interrupt_scroll_smoke() {
        let (effects, sink) = scripted();
        let opts = TuiOptions { workspace: Some("/tmp/demo".into()) };
        let mut app = App::new(opts, sink, 80, 24);
        app.apply(Action::Connected(true), 0);
        open_s1(&mut app);

        // -- type --
        type_text(&mut app, "add a test", 10);
        let typed = frame_of(&app, 80, 24);
        assert!(typed.contains("add a test"), "{typed}");
        assert!(typed.contains("one program per round"), "{typed}");

        // -- send --
        app.apply(key(KeyCode::Enter), 20);
        assert_eq!(sends(&effects), vec![Effect::Send("add a test".into())]);
        let sent = frame_of(&app, 80, 24);
        assert!(sent.contains("you"), "{sent}");
        assert!(sent.contains("type a message · enter sends"), "composer cleared: {sent}");

        // -- stream --
        app.apply(
            event(
                EventType::MessageStarted,
                1_000,
                json!({
                    "id": "m1", "sessionId": "s1", "role": "supervisor",
                    "parts": [], "pending": true, "createdAt": 1000
                }),
            ),
            1_000,
        );
        assert!(app.busy());
        app.apply(
            event(EventType::MessageDelta, 1_100, json!({"messageId": "m1", "delta": "Working on"})),
            1_100,
        );
        app.apply(
            event(EventType::MessageDelta, 1_200, json!({"messageId": "m1", "delta": " it now."})),
            1_200,
        );
        let streaming = frame_of(&app, 80, 24);
        assert!(streaming.contains("Working on it now.▌"), "{streaming}");
        assert!(streaming.contains("esc interrupts"), "{streaming}");
        assert!(streaming.contains("bough"), "{streaming}");

        // -- interrupt (esc while busy fires immediately, never held) --
        // OUTSIDE the take-back window, deliberately: inside it Escape is the
        // take-back, which stops the turn on its way out (keys.rs) — that
        // ordering has its own test below.
        app.apply(key(KeyCode::Esc), 20 + crate::keys::UNSEND_MS + 1);
        assert_eq!(
            sends(&effects),
            vec![Effect::Send("add a test".into()), Effect::Interrupt]
        );
        app.apply(
            event(
                EventType::TurnFinished,
                2_100,
                json!({"turnId": "t1", "sessionId": "s1", "status": "interrupted"}),
            ),
            2_100,
        );
        assert!(!app.busy());
        let settled = frame_of(&app, 80, 24);
        assert!(!settled.contains("esc interrupts"), "{settled}");

        // -- scroll (offset counts up from the live tail) --
        for i in 0..40 {
            app.apply(
                event(
                    EventType::MessagePart,
                    3_000 + i,
                    json!({"messageId": "m1", "part": {"type": "text", "text": format!("filler {i}")}}),
                ),
                3_000 + i,
            );
        }
        app.apply(key(KeyCode::PageUp), 4_000);
        let scrolled = frame_of(&app, 80, 24);
        assert!(scrolled.contains("more lines below ·"), "{scrolled}");
        app.apply(key(KeyCode::Esc), 4_100); // cancel resets the scroll
        assert_eq!(app.scroll_off, 0);
    }

    #[test]
    fn finalized_text_part_replaces_the_streamed_copy() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(
            event(
                EventType::MessageStarted,
                1_000,
                json!({
                    "id": "m1", "sessionId": "s1", "role": "supervisor",
                    "parts": [], "pending": true, "createdAt": 1000
                }),
            ),
            1_000,
        );
        app.apply(
            event(EventType::MessageDelta, 1_100, json!({"messageId": "m1", "delta": "Done."})),
            1_100,
        );
        app.apply(
            event(
                EventType::MessagePart,
                1_200,
                json!({"messageId": "m1", "part": {"type": "text", "text": "Done."}}),
            ),
            1_200,
        );
        let lines = app.transcript_lines();
        let hits = lines.iter().filter(|l| l.contains("Done.")).count();
        assert_eq!(hits, 1, "live lines are replaced by the part, not duplicated: {lines:?}");
        assert!(!lines.iter().any(|l| l.contains('▌')));
    }

    #[test]
    fn retry_drops_the_partial_stream() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(
            event(
                EventType::MessageStarted,
                1_000,
                json!({
                    "id": "m1", "sessionId": "s1", "role": "supervisor",
                    "parts": [], "pending": true, "createdAt": 1000
                }),
            ),
            1_000,
        );
        app.apply(
            event(EventType::MessageDelta, 1_100, json!({"messageId": "m1", "delta": "half a"})),
            1_100,
        );
        app.apply(
            event(
                EventType::MessageRetry,
                1_200,
                json!({"messageId": "m1", "attempt": 1, "reason": "overloaded"}),
            ),
            1_200,
        );
        assert!(!app.transcript_lines().iter().any(|l| l.contains("half a")));
    }

    #[test]
    fn double_esc_clears_the_draft_but_a_stop_is_never_delayed() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        type_text(&mut app, "draft", 0);

        // Idle: first esc arms, second within the window clears.
        app.apply(key(KeyCode::Esc), 100);
        assert_eq!(app.draft, "draft");
        app.apply(key(KeyCode::Esc), 300);
        assert_eq!(app.draft, "");

        // Busy: esc interrupts even with a draft — the rewind never shadows the stop.
        type_text(&mut app, "draft2", 400);
        app.apply(
            event(
                EventType::MessageStarted,
                500,
                json!({
                    "id": "m1", "sessionId": "s1", "role": "supervisor",
                    "parts": [], "pending": true, "createdAt": 500
                }),
            ),
            500,
        );
        app.apply(key(KeyCode::Esc), 600);
        assert_eq!(sends(&effects), vec![Effect::Interrupt]);
        assert_eq!(app.draft, "draft2", "the draft survives a stop");
    }

    // ---- the rail, the job view, the ask card, the take-back ------------

    fn job(id: &str, command: &str) -> BackgroundJob {
        BackgroundJob {
            id: id.into(),
            name: id.into(),
            session_id: "s1".into(),
            pid: 4242,
            command: command.into(),
            status: bough_core::schema::parts::JobStatus::Running,
            exit_code: None,
            signal: None,
            started_at: 0,
            exited_at: None,
        }
    }

    /// The rail is FED — a poll's rows reach the screen, and the job's own
    /// view opens on it. This is the wiring the components were missing.
    #[test]
    fn a_polled_job_reaches_the_rail_and_enter_opens_its_output() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(Action::Jobs(vec![job("job-1", "sleep 30")]), 5_000);
        let railed = frame_of(&app, 80, 24);
        assert!(railed.contains("sleep 30"), "the rail shows the shell: {railed}");

        // ↓ from an empty composer enters the rail; ⏎ opens the job.
        app.apply(key(KeyCode::Down), 5_100);
        assert_eq!(app.rail_sel, Some(0));
        app.apply(key(KeyCode::Enter), 5_200);
        assert!(sends(&effects).contains(&Effect::LoadJobOutput("job-1".into())));
        app.apply(
            Action::JobOutput {
                id: "job-1".into(),
                output: "hello from the shell\n".into(),
                job: Some(job("job-1", "sleep 30")),
                error: None,
            },
            5_300,
        );
        let opened = frame_of(&app, 80, 24);
        assert!(opened.contains("hello from the shell"), "{opened}");
        // x arms, x again kills — consent is never inferred.
        app.apply(key(KeyCode::Char('x')), 5_400);
        assert!(sends(&effects).iter().all(|e| *e != Effect::KillJob("job-1".into())));
        app.apply(key(KeyCode::Char('x')), 5_500);
        assert!(sends(&effects).contains(&Effect::KillJob("job-1".into())));
        // esc returns to the rail, not to the composer.
        app.apply(key(KeyCode::Esc), 5_600);
        assert!(app.job.is_none());
        assert_eq!(app.rail_sel, Some(0));
    }

    /// A pending hold renders with its options and is answerable.
    #[test]
    fn a_pending_ask_renders_a_card_and_a_digit_answers_it() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(
            event(
                EventType::AskQuestion,
                1_000,
                json!({
                    "id": "q1", "sessionId": "s1", "messageId": "m1",
                    "question": "which colour do you prefer?",
                    "options": ["blue", "green"],
                    "status": "pending", "ts": 1000
                }),
            ),
            1_000,
        );
        let card = frame_of(&app, 80, 24);
        assert!(card.contains("which colour do you prefer?"), "{card}");
        assert!(card.contains("blue") && card.contains("green"), "{card}");
        // The card owns the keyboard: typing goes into IT, not the composer.
        type_text(&mut app, "teal", 1_100);
        assert_eq!(app.ask_typed, "teal");
        assert_eq!(app.draft, "");
        // …and a digit picks the option it is numbered with.
        app.apply(key(KeyCode::Char('2')), 1_200);
        assert!(sends(&effects).contains(&Effect::AnswerAsk {
            session_id: "s1".into(),
            id: "q1".into(),
            answer: "green".into(),
        }));
        assert!(app.ask.is_none(), "the card comes down when it is answered");
    }

    /// The gate: INSIDE the window Escape takes the message back rather than
    /// stopping the turn — and outside it, it stops.
    #[test]
    fn inside_the_window_the_take_back_outranks_the_stop() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        type_text(&mut app, "ship it", 0);
        app.apply(key(KeyCode::Enter), 1_000);
        // The window is on screen, said out loud.
        let armed = frame_of(&app, 80, 24);
        assert!(armed.contains(TAKE_BACK_HINT), "{armed}");
        // The server's copy of the message, so there is something to unsend.
        app.apply(
            event(
                EventType::MessageStarted,
                1_010,
                json!({
                    "id": "u1", "sessionId": "s1", "role": "user",
                    "parts": [{"type": "text", "text": "ship it"}],
                    "pending": false, "createdAt": 1010
                }),
            ),
            1_010,
        );
        app.apply(
            event(
                EventType::MessageStarted,
                1_020,
                json!({
                    "id": "m1", "sessionId": "s1", "role": "supervisor",
                    "parts": [], "pending": true, "createdAt": 1020
                }),
            ),
            1_020,
        );
        app.apply(key(KeyCode::Esc), 1_000 + crate::keys::UNSEND_MS - 1);
        assert_eq!(sends(&effects).last(), Some(&Effect::Unsend("u1".into())));
        // The text comes back to the composer, with the notice that says so.
        app.apply(Action::TookBack("ship it".into()), 1_100);
        assert_eq!(app.draft, "ship it");
        assert_eq!(app.cursor, 7);
        let back = frame_of(&app, 80, 24);
        assert!(back.contains("took that back"), "{back}");

        // Outside the window the same key is the stop.
        app.apply(key(KeyCode::Esc), 1_000 + crate::keys::UNSEND_MS + 1);
        assert_eq!(sends(&effects).last(), Some(&Effect::Interrupt));
    }

    /// The idle-tick skip must not swallow the poll's repaint.
    #[test]
    fn a_live_job_keeps_the_screen_repainting_when_no_turn_is_running() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        assert!(!app.animating(), "an idle screen still stops the redraw loop");
        app.apply(Action::Jobs(vec![job("job-1", "sleep 30")]), 1_000);
        assert!(!app.busy());
        assert!(app.animating(), "a running shell is a reason to repaint");
    }

    #[test]
    fn ctrl_c_is_a_two_press_quit_and_any_other_chord_disarms() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        app.apply(ctrl('c'), 0);
        assert!(!app.quit);
        assert!(app
            .notice
            .as_deref()
            .unwrap()
            .contains("^c again to quit — subagents and workflows keep running"));
        app.apply(key(KeyCode::Char('x')), 10); // disarms
        app.apply(ctrl('c'), 20);
        assert!(!app.quit, "a lone ^c after a disarm must re-arm, not quit");
        app.apply(ctrl('c'), 30);
        assert!(app.quit);
    }

    #[test]
    fn sigils_never_reach_the_model_and_keep_the_draft() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        type_text(&mut app, "!echo hi", 0);
        app.apply(key(KeyCode::Enter), 10);
        assert!(effects.borrow().is_empty(), "a ! line must not bill the model");
        assert_eq!(app.draft, "!echo hi");

        app.apply(key(KeyCode::Esc), 20);
        app.apply(key(KeyCode::Esc), 30); // clear
        // An unrecognised `/word` is a command ATTEMPT, never prose: it is
        // intercepted with the teaching error and the draft is kept.
        type_text(&mut app, "/zzz", 40);
        app.apply(key(KeyCode::Enter), 50);
        assert!(sends(&effects).is_empty(), "{:?}", effects.borrow());
        assert!(app.notice.as_deref().unwrap().contains("there is no /zzz"), "{:?}", app.notice);
        assert_eq!(app.draft, "/zzz", "the draft is kept so it can be edited");

        // A message that merely BEGINS with a command is still a message.
        app.apply(key(KeyCode::Esc), 60);
        app.apply(key(KeyCode::Esc), 70);
        type_text(&mut app, "/model is the wrong word here", 80);
        app.apply(key(KeyCode::Enter), 90);
        assert_eq!(
            sends(&effects).as_slice(),
            &[Effect::Send("/model is the wrong word here".into())]
        );
    }

    // ---- row 2.22: composer intelligence -----------------------------------

    fn with_files<T: Transport>(app: &mut App<T>, files: &[&str]) {
        app.apply(
            Action::Files(files.iter().map(|f| f.to_string()).collect()),
            0,
        );
    }

    /// The measured bug this prevents: `/model` typed or pasted with its Return
    /// in one read never opened the popup, so Enter sent it to the frontier
    /// model as prose — 19k tokens billed and a session titled "Model
    /// Architecture Discussion". Dispatch runs at SEND time.
    #[test]
    fn a_slash_command_dispatches_at_send_time_and_is_never_sent_to_the_model() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        // A paste arrives with no popup ever opening — set the draft the way a
        // fast typist's chunk does, then press Return.
        type_text(&mut app, "/model", 0);
        app.apply(key(KeyCode::Esc), 5); // dismiss the popup: this is the send path
        app.apply(key(KeyCode::Enter), 10);
        assert_eq!(
            sends(&effects).as_slice(),
            &[Effect::Run(Command::Tab(crate::keys::PanelTab::Model), String::new())]
        );
        assert_eq!(app.draft, "", "a dispatched command leaves nothing behind");

        // …and an argument reaches the commands that declare one.
        type_text(&mut app, "/compact focus on the parser", 20);
        app.apply(key(KeyCode::Enter), 30);
        assert_eq!(
            sends(&effects).last().unwrap(),
            &Effect::Run(Command::SessionCompact, "focus on the parser".into())
        );
    }

    /// `/clear`, typed out of Claude Code habit, once reached haiku — which
    /// answered "Done. State cleared." and offered to revert the workspace's
    /// modified files. A confirmation for something that never happened.
    #[test]
    fn an_unknown_slash_word_is_intercepted_with_the_name_that_does_exist() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        type_text(&mut app, "/clear", 0);
        app.apply(key(KeyCode::Esc), 5);
        app.apply(key(KeyCode::Enter), 10);
        assert!(sends(&effects).is_empty(), "{:?}", effects.borrow());
        let notice = app.notice.clone().unwrap();
        assert!(notice.contains("there is no /clear"), "{notice}");
        assert!(notice.contains("did you mean /new?"), "{notice}");
        assert!(notice.contains("type / for the list, or ? for every key"), "{notice}");
        assert_eq!(app.draft, "/clear");
    }

    #[test]
    fn the_at_popup_ranks_the_workspace_listing_and_enter_inserts_the_path() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        type_text(&mut app, "look at @app", 0);
        // The listing is fetched once, lazily, when the marker is first typed.
        assert!(effects.borrow().contains(&Effect::LoadFiles));
        with_files(&mut app, &["server/app.ts", "app.tsx", "components/Chat.tsx"]);

        let frame = frame_of(&app, 80, 24);
        assert!(frame.contains("@app.tsx"), "exact prefix leads: {frame}");
        assert!(frame.contains("files & dirs"), "{frame}");
        assert!(!frame.contains("Chat.tsx"), "a non-match is not a row: {frame}");

        // ⏎ belongs to the popup while it is open — it inserts, never sends.
        app.apply(key(KeyCode::Enter), 10);
        assert!(sends(&effects).is_empty(), "{:?}", effects.borrow());
        assert_eq!(app.draft, "look at @app.tsx ");
        assert_eq!(app.cursor, "look at @app.tsx ".chars().count());
    }

    #[test]
    fn the_popup_owns_up_down_tab_and_esc_while_it_is_open() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        type_text(&mut app, "@app", 0);
        with_files(&mut app, &["app.tsx", "app.md", "app.rs"]);

        app.apply(key(KeyCode::Down), 10);
        app.apply(key(KeyCode::Down), 11);
        app.apply(key(KeyCode::Up), 12);
        assert_eq!(app.completion_sel, 1);
        // ⇥ accepts the highlighted row (the ghost never sees it while a popup
        // is open).
        app.apply(key(KeyCode::Tab), 13);
        assert!(app.draft.starts_with('@'));
        assert_ne!(app.draft, "@app", "tab accepted a row");

        // esc closes the popup and NOTHING else — the draft survives and no
        // interrupt is fired: escape unwinds exactly one level.
        type_text(&mut app, " @app", 20);
        assert!(app.completing());
        app.apply(key(KeyCode::Esc), 21);
        assert!(!app.completing(), "esc dismissed the popup");
        assert!(app.draft.ends_with(" @app"), "{}", app.draft);
        assert!(sends(&effects).is_empty(), "esc in a popup is not a stop");
        // …and typing re-opens it: esc dismissed THIS token, not completion.
        type_text(&mut app, ".", 22);
        assert!(app.trigger().is_some());
    }

    #[test]
    fn a_path_that_leaves_the_workspace_browses_the_filesystem_instead_of_git() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        with_files(&mut app, &["src/tui/app.rs"]);
        type_text(&mut app, "@~/rep", 0);
        // `git ls-files` cannot name anything outside the repo, so this fetches
        // ONE directory instead — the one already visible in what was typed.
        assert!(
            effects.borrow().contains(&Effect::LoadDirEntries("~/".into())),
            "{:?}",
            effects.borrow()
        );
        // Until the entries for THIS prefix land, the popup is honestly empty.
        assert!(app.completion().items.is_empty());

        app.apply(
            Action::DirEntries {
                prefix: "~/".into(),
                entries: vec!["repos/".into(), "Desktop/".into(), ".zshrc".into()],
            },
            10,
        );
        let items = app.completion().items;
        assert_eq!(items[0].label, "@~/repos/");
        // A directory keeps its slash and gains no trailing space, so accepting
        // it drills one level down instead of ending the reference.
        app.apply(key(KeyCode::Enter), 20);
        assert_eq!(app.draft, "@~/repos/");
        assert!(
            effects.borrow().contains(&Effect::LoadDirEntries("~/repos/".into())),
            "accepting a directory fetches the next level: {:?}",
            effects.borrow()
        );
    }

    #[test]
    fn the_slash_popup_leads_with_the_built_ins_and_a_command_row_runs() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        app.apply(
            Action::Skills(vec![("prewalk".into(), "plan first, then edit".into())]),
            0,
        );
        type_text(&mut app, "/tre", 5);
        assert!(effects.borrow().contains(&Effect::LoadSkills));
        let items = app.completion().items;
        assert_eq!(items[0].label, "/tree");
        assert!(items[0].run.is_some(), "a built-in row carries its command");
        // ⏎ RUNS it, and the token comes out of the draft rather than being
        // left behind as text in the next message.
        app.apply(key(KeyCode::Enter), 10);
        assert_eq!(app.draft, "");
        assert_eq!(
            sends(&effects).as_slice(),
            &[Effect::Run(Command::Tab(crate::keys::PanelTab::Tree), String::new())]
        );

        // A SKILL row is a reference the model reads: it inserts, never runs.
        type_text(&mut app, "/prew", 20);
        let item = app.completion().items[0].clone();
        assert_eq!(item.label, "/prewalk");
        assert_eq!(item.run, None);
        app.apply(key(KeyCode::Tab), 21);
        assert_eq!(app.draft, "/prewalk ");
        assert_eq!(sends(&effects).len(), 1, "a skill row dispatches nothing");
    }

    #[test]
    fn a_trigger_that_matches_nothing_still_draws_but_does_not_own_the_keys() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        with_files(&mut app, &["src/app.rs"]);
        type_text(&mut app, "@zzzz", 0);
        let frame = frame_of(&app, 80, 24);
        assert!(frame.contains("no matching files"), "{frame}");
        assert!(!app.completing(), "an empty popup must not swallow ⏎");
        app.apply(key(KeyCode::Enter), 10);
        assert_eq!(sends(&effects).as_slice(), &[Effect::Send("@zzzz".into())]);
    }

    #[test]
    fn wheel_scrolls_and_clamps() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        for i in 0..5 {
            app.apply(
                event(
                    EventType::MessageStarted,
                    i,
                    json!({
                        "id": format!("m{i}"), "sessionId": "s1", "role": "user",
                        "parts": [{"type": "text", "text": "hello"}],
                        "pending": false, "createdAt": i
                    }),
                ),
                i,
            );
        }
        let wheel_up = Action::Term(TermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }));
        app.apply(wheel_up, 100);
        assert_eq!(app.scroll_off, WHEEL_ROWS);
        for _ in 0..50 {
            let up = Action::Term(TermEvent::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }));
            app.apply(up, 200);
        }
        assert_eq!(app.scroll_off, app.transcript_lines().len() - 1, "clamped to lines-1");
    }

    // ---- mouse selection and click (row 2.25) ------------------------------

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Action {
        Action::Term(TermEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }))
    }

    type Captured = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    /// An app whose copy and open paths are recorded instead of performed.
    fn app_with_capture() -> (App<impl FnMut(Effect)>, Captured, Captured) {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        let copied: Captured = Default::default();
        let opened: Captured = Default::default();
        let (c, o) = (copied.clone(), opened.clone());
        app.set_copy(Box::new(move |t| c.lock().unwrap().push(t.to_string())));
        app.set_open(Box::new(move |u| o.lock().unwrap().push(u.to_string())));
        (app, copied, opened)
    }

    fn recorded(c: &Captured) -> Vec<String> {
        c.lock().unwrap().clone()
    }

    #[test]
    fn a_drag_copies_on_release_and_says_how_much() {
        let (mut app, copied, _opened) = app_with_capture();
        open_s1(&mut app);
        type_text(&mut app, "hello selection", 0);
        app.apply(key(KeyCode::Enter), 10);
        // Drag across the composer's own row — whatever is painted there is
        // what a terminal's selection would have taken.
        let row = 20u16;
        app.apply(mouse(MouseEventKind::Down(MouseButton::Left), 0, row), 20);
        app.apply(mouse(MouseEventKind::Drag(MouseButton::Left), 10, row), 21);
        app.apply(mouse(MouseEventKind::Up(MouseButton::Left), 20, row), 22);
        // Either something was on those cells (then it was copied and said so)
        // or nothing was (then nothing is claimed) — never a silent copy.
        let said = app.notice.as_deref().unwrap_or("");
        if said.starts_with("copied ") {
            assert_eq!(recorded(&copied).len(), 1, "exactly one clipboard write");
            let n = recorded(&copied)[0].chars().count();
            assert_eq!(said, format!("copied {n} characters"));
        } else {
            assert!(recorded(&copied).is_empty(), "an empty drag copies nothing");
        }
        assert!(app.sel.is_none(), "the highlight is dropped on release");
    }

    #[test]
    fn a_press_and_release_in_place_is_a_click_not_a_copy() {
        let (mut app, copied, _opened) = app_with_capture();
        app.apply(mouse(MouseEventKind::Down(MouseButton::Left), 3, 3), 0);
        app.apply(mouse(MouseEventKind::Up(MouseButton::Left), 3, 3), 1);
        assert!(recorded(&copied).is_empty(), "a click is not a drag");
        assert_eq!(app.notice, None);
    }

    #[test]
    fn a_click_on_a_url_opens_it_and_a_click_beside_it_does_not() {
        let (mut app, _copied, opened) = app_with_capture();
        open_s1(&mut app);
        // A transcript row carrying a bare address.
        app.thread.push(Message {
            id: "m-url".into(),
            session_id: "s1".into(),
            role: Role::Supervisor,
            parts: vec![Part::Text { text: "see https://example.com/auth now".into() }],
            pending: false,
            created_at: 1,
        });
        let painted = app.painted_rows();
        let (y, col) = painted
            .iter()
            .enumerate()
            .find_map(|(i, row)| row.find("https://").map(|b| (i, row[..b].chars().count())))
            .expect("the address is on screen");
        app.apply(mouse(MouseEventKind::Down(MouseButton::Left), col as u16, y as u16), 0);
        app.apply(mouse(MouseEventKind::Up(MouseButton::Left), col as u16, y as u16), 1);
        assert_eq!(recorded(&opened).as_slice(), &["https://example.com/auth".to_string()]);

        // One column before the address is prose, and prose opens nothing.
        app.apply(mouse(MouseEventKind::Down(MouseButton::Left), 0, y as u16), 2);
        app.apply(mouse(MouseEventKind::Up(MouseButton::Left), 0, y as u16), 3);
        assert_eq!(recorded(&opened).len(), 1, "a click on prose opened nothing");
    }

    #[test]
    fn only_http_and_https_are_ever_handed_to_the_platform_opener() {
        // Transcript URLs are model-written; `open file:///…` would run whatever
        // the desktop has registered. The boundary is here, not in the hit-test.
        let (mut app, _copied, opened) = app_with_capture();
        app.open_link("file:///etc/passwd");
        app.open_link("vscode://x");
        assert!(recorded(&opened).is_empty(), "{:?}", recorded(&opened));
        app.open_link("https://example.com");
        assert_eq!(recorded(&opened).len(), 1);
    }

    #[test]
    fn another_sessions_events_never_stream_into_this_thread() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        let mut foreign = BoughEvent {
            r#type: EventType::MessageStarted,
            session_id: Some("s2".into()),
            seq: 1,
            ts: 1_000,
            data: json!({
                "id": "m9", "sessionId": "s2", "role": "supervisor",
                "parts": [], "pending": true, "createdAt": 1000
            }),
        };
        app.apply(Action::Event(foreign.clone()), 1_000);
        assert!(!app.busy(), "a foreign session's turn must not mark this one busy");
        assert!(app.transcript_lines().is_empty());
        // Un-scoped events still pass.
        foreign.session_id = None;
        foreign.r#type = EventType::SessionActivity;
        foreign.data = json!({"sessionId": "s1", "activity": "compiling"});
        app.apply(Action::Event(foreign), 1_100);
        assert_eq!(app.activity.as_deref(), Some("compiling"));
    }

    // ---- the one panel, wired (row 2.20) -----------------------------------

    fn sessions() -> Vec<crate::api::SessionRow> {
        use crate::forest::fixtures::session_row;
        use bough_core::schema::parts::SessionKind;
        let mut a = session_row("s1", SessionKind::Root, 1_000);
        a.session.title = "wire the panel".into();
        let mut b = session_row("s2", SessionKind::Root, 2_000);
        b.session.title = "nightly bench".into();
        vec![a, b]
    }

    #[test]
    fn ctrl_t_opens_the_panel_on_the_tree_and_the_second_press_closes_it() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        app.apply(ctrl('t'), 0);
        assert!(app.panel.open());
        assert_eq!(app.panel.tab(), crate::keys::PanelTab::Tree);
        // Entry FETCHES: an empty tree that never asked is indistinguishable
        // from a machine with no conversations.
        assert!(sends(&effects).contains(&Effect::LoadSessions));
        app.apply(ctrl('t'), 0);
        assert!(!app.panel.open());
    }

    #[test]
    fn a_tab_chord_jumps_straight_there_and_the_composer_says_who_has_the_keyboard() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        app.apply(ctrl('d'), 0); // ^d — the changes tab, guarded on an empty draft
        assert_eq!(app.panel.tab(), crate::keys::PanelTab::Changes);
        assert!(sends(&effects).contains(&Effect::LoadChanges));
        let frame = frame_of(&app, 80, 24);
        assert!(frame.contains("[changes]"), "{frame}");
        assert!(frame.contains("has the keyboard · esc returns here"), "{frame}");
        // The chord that brought you here takes you back.
        app.apply(ctrl('d'), 0);
        assert!(!app.panel.open());
    }

    #[test]
    fn a_composer_owned_chord_is_a_tab_jump_only_on_an_empty_draft() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        type_text(&mut app, "hello", 0);
        app.apply(ctrl('f'), 0); // ^f with a draft is `cursor.right`, not the tree
        assert!(!app.panel.open());
        assert_eq!(app.cursor, 5);
    }

    #[test]
    fn the_panel_displaces_the_transcript_rather_than_floating_over_it() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(
            event(EventType::MessageStarted, 1_000, json!({
                "id": "m1", "sessionId": "s1", "role": "user",
                "parts": [{"type": "text", "text": "a transcript row"}],
                "pending": false, "createdAt": 1_000,
            })),
            1_000,
        );
        assert!(frame_of(&app, 80, 24).contains("a transcript row"));
        app.apply(ctrl('t'), 0);
        app.apply(Action::Sessions(sessions()), 0);
        let frame = frame_of(&app, 80, 24);
        assert!(!frame.contains("a transcript row"), "the panel must displace the chat: {frame}");
        assert!(frame.contains("nightly bench"), "{frame}");
        // …and the composer and status line stay pinned below it.
        assert!(frame.contains("? help"), "{frame}");
    }

    #[test]
    fn enter_on_a_tree_row_opens_that_conversation_and_closes_the_panel() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        app.apply(ctrl('t'), 0);
        app.apply(Action::Sessions(sessions()), 0);
        app.apply(key(KeyCode::Down), 0); // newest first: s2, then s1
        app.apply(key(KeyCode::Enter), 0);
        assert!(sends(&effects).contains(&Effect::OpenSession("s1".into())));
        assert!(!app.panel.open(), "the switcher closes when it switches");
    }

    #[test]
    fn a_slash_command_reaches_the_panel_it_names_rather_than_the_model() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        type_text(&mut app, "/tree", 0);
        app.apply(key(KeyCode::Enter), 0);
        // The dispatch stays ONE funnel: the effect goes out…
        assert_eq!(
            sends(&effects),
            &[Effect::Run(Command::Tab(crate::keys::PanelTab::Tree), String::new())]
        );
        assert!(!app.panel.open());
        // …and the transport hands the client-owned ones straight back.
        app.apply(Action::Run(Command::Tab(crate::keys::PanelTab::Tree)), 0);
        assert!(app.panel.open());
        assert_eq!(app.panel.tab(), crate::keys::PanelTab::Tree);
    }

    #[test]
    fn the_help_overlay_opens_on_a_bare_question_mark_and_is_the_whole_screen() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions { workspace: Some("/w/demo".into()) }, sink, 80, 24);
        app.apply(key(KeyCode::Char('?')), 0);
        let frame = frame_of(&app, 80, 24);
        assert!(frame.starts_with("keys · esc closes"), "{frame}");
        // It DISPLACES everything — the header and the composer included.
        assert!(!frame.contains("demo"), "{frame}");
        assert!(frame.contains("↑↓ pgup/pgdn scroll"), "{frame}");
        // …and esc closes it.
        app.apply(key(KeyCode::Esc), 0);
        assert!(!app.help_open);
        assert!(frame_of(&app, 80, 24).contains("demo"));
    }

    #[test]
    fn no_surface_may_swallow_the_two_press_quit() {
        // ^c is bound in every mode and is the one way out of a wedged
        // terminal; a panel that ate it would strand the user inside it.
        for open in ["ctrl+t", "?"] {
            let (_effects, sink) = scripted();
            let mut app = App::new(TuiOptions::default(), sink, 80, 24);
            app.apply(if open == "?" { key(KeyCode::Char('?')) } else { ctrl('t') }, 0);
            app.apply(ctrl('c'), 0);
            assert!(!app.quit, "one ^c must never tear the UI down");
            assert!(app.notice.as_deref().unwrap_or("").contains("^c again to quit"));
            app.apply(ctrl('c'), 0);
            assert!(app.quit, "^c ^c must quit from {open}");
        }
    }

    #[test]
    fn a_question_mark_inside_a_draft_is_a_character_not_the_overlay() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        type_text(&mut app, "why", 0);
        app.apply(key(KeyCode::Char('?')), 0);
        assert!(!app.help_open);
        assert_eq!(app.draft, "why?");
    }

    #[test]
    fn the_overlay_scrolls_and_never_runs_past_its_own_end() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        app.apply(key(KeyCode::Char('?')), 0);
        app.apply(key(KeyCode::Down), 0);
        assert_eq!(app.help_off, HELP_STEP);
        for _ in 0..200 {
            app.apply(key(KeyCode::PageDown), 0);
        }
        let total = overlay_lines().len();
        assert_eq!(app.help_off, clamp_help_offset(usize::MAX, total, 24));
        // An unclamped offset blanks the overlay; this one still paints rows.
        let frame = frame_of(&app, 80, 24);
        assert!(frame.contains("↑↓ pgup/pgdn scroll · end"), "{frame}");
    }

    #[test]
    fn the_changes_tab_arms_a_revert_and_the_second_press_performs_it() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(ctrl('d'), 0);
        app.apply(
            Action::Changes(Some(crate::store::state::SessionChangeSet {
                available: true,
                reason: None,
                base: Some("abcdef1234".into()),
                files: vec![json!({
                    "path": "src/a.ts",
                    "status": "modified",
                    "hunks": [{"header": "@@ -1 +1 @@", "lines": ["-a", "+b"]}],
                })],
                workspace: Some("/tmp/x".into()),
            })),
            0,
        );
        assert!(frame_of(&app, 80, 24).contains("src/a.ts"));
        // `x` ARMS and prints the blast radius; nothing has been sent.
        app.apply(key(KeyCode::Char('x')), 0);
        let armed = frame_of(&app, 80, 24);
        assert!(armed.contains("revert src/a.ts?"), "{armed}");
        assert!(armed.contains("DISCARDS +1 -1"), "{armed}");
        assert!(!sends(&effects).iter().any(|e| matches!(e, Effect::Revert(_))));
        // ⏎ performs it, addressed to the path.
        app.apply(key(KeyCode::Enter), 0);
        assert!(sends(&effects)
            .contains(&Effect::Revert(Some(vec!["src/a.ts".to_string()]))));
    }

    #[test]
    fn the_revert_outcome_names_every_path_and_which_way_it_went() {
        let outcome = crate::api::RevertOutcome {
            reverted: vec!["a.ts".into()],
            skipped: vec!["b.ts".into()],
            failed: vec![crate::api::RevertFailure {
                path: "c.ts".into(),
                error: "unmerged".into(),
            }],
        };
        assert_eq!(
            revert_outcome(&outcome),
            "reverted a.ts · not in this change set: b.ts · failed c.ts: unmerged"
        );
        assert_eq!(
            revert_outcome(&crate::api::RevertOutcome::default()),
            "nothing was reverted"
        );
    }

    #[test]
    fn with_no_conversation_the_changes_tab_does_not_claim_a_checkout_it_has_no_view_of() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        app.apply(ctrl('d'), 0);
        app.apply(Action::Changes(Some(no_session_changes())), 0);
        let frame = frame_of(&app, 80, 24);
        assert!(frame.contains("no conversation is open"), "{frame}");
        assert!(!frame.contains("the agent still works here"), "{frame}");
    }

    #[test]
    fn disconnected_suffix_rides_the_header() {
        let (_effects, sink) = scripted();
        let mut app =
            App::new(TuiOptions { workspace: Some("/w/demo".into()) }, sink, 80, 24);
        let down = frame_of(&app, 80, 24);
        assert!(down.contains("demo  · disconnected"), "{down}");
        app.apply(Action::Connected(true), 0);
        let up = frame_of(&app, 80, 24);
        assert!(!up.contains("disconnected"), "{up}");
    }
}
