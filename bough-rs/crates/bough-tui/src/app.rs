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

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

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
use crate::components::composer::{
    completion_popup_height, composer_height, render_completion_popup, render_composer,
    CompletionPopupProps, ComposerProps,
};
use crate::components::help::{clamp_help_offset, overlay_lines, render_help, HELP_STEP};
use crate::components::job_output::{
    job_body_rows, job_sub_lines, render_job_output, JobOutputProps,
};
use crate::components::panel::host::{HostRequest, PanelHost};
use crate::components::panel::{panel_body_rows, render_panel, PanelBody};
use crate::components::rail::{live_subagents, rail_rows, render_rail};
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
    is_empty_selection, link_at, row_content, selected_copy, url_across, url_at, CopyRow, Point,
    Selection,
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
    DirEntries {
        prefix: String,
        entries: Vec<String>,
    },
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
    /// the client answers itself return here. The argument travels with it:
    /// `/compact <goal>` is the one command whose trailing text is an ARGUMENT
    /// rather than prose, and dropping it here would silently discard the
    /// steer the user typed.
    Run(Command, String),
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
    /// Text the server wrote FOR the composer: a handoff's distilled prompt, a
    /// forked user turn's own words. It is placed, never sent — the user reads
    /// what was carried over and edits it before any of it goes anywhere.
    Draft(String),
    /// `GET /schedules` — the rail's standing promises, disabled rows included.
    Schedules(Vec<bough_core::schema::parts::Schedule>),
    /// `POST /sessions/:id/ghost` — the cheap tier's guess at the next message,
    /// or the empty string for every failure there is.
    Ghost(String),
    /// `POST /sessions/:id/sections` — topic headers over one conversation.
    Sections {
        session_id: String,
        sections: Vec<crate::forest::SectionRange>,
    },
    /// `GET /search` — the conversations and turns the tree's `/` matched. `q`
    /// travels with them so a stale reply cannot mark the wrong rows.
    SearchHits {
        q: String,
        sessions: Vec<String>,
        messages: Vec<String>,
    },
    /// `GET /workflows` — the workflows tab's run list.
    Workflows(Vec<crate::api::WorkflowSummary>),
    /// `GET /workflows/:id` — one run's whole view. `None` is a failed fetch,
    /// which drops the view back to the list rather than painting zeroes.
    Workflow(Option<crate::api::WorkflowDetail>),
    /// `GET /mcp/servers` — registry, grants, connections. NEVER cached: it is
    /// re-fetched on every entry into the tab.
    Mcp(Option<crate::api::McpStatus>),
    /// `GET /skills` — the full rows AND the directories that were walked.
    /// `skills: None` is a failed fetch and carries its reason.
    SkillRows {
        skills: Option<Vec<crate::components::panel::skills::SkillRow>>,
        sources: Vec<crate::api::SkillSourceRow>,
        note: Option<String>,
    },
    /// `GET /models` — the picker's catalog.
    Models(Vec<crate::api::ModelRow>),
    /// `GET /model-settings` — what a NEW conversation runs on, both tiers.
    ModelSettings(crate::api::ModelSettings),
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
    AnswerAsk {
        session_id: String,
        id: String,
        answer: String,
    },
    DeclineAsk {
        session_id: String,
        id: String,
    },
    /// The posted take-back: delete this message (and what followed) and stop
    /// the turn it started, in ONE call.
    Unsend(String),
    /// Ask the cheap tier what the user is about to type. Debounced, and every
    /// failure is silence.
    GhostText(String),
    /// Ask the cheap tier where this conversation changed subject.
    Sections {
        session_id: String,
        gists: Vec<String>,
    },
    /// Full-text search behind the tree's `/`.
    SearchSessions(String),
    // ---- row 3.20: the four remaining tabs, each fetch injected -------------
    /// `GET /workflows?session=` — the run list, on entry to the tab.
    LoadWorkflows,
    /// `GET /workflows/:id` — one run's view, on open and after a steer.
    LoadWorkflow(String),
    /// One steering verb, then a re-read: the answer to "did it pause" is the
    /// run's own state, never the POST's 202.
    SteerWorkflow {
        id: String,
        action: crate::components::panel::host::WorkflowAction,
    },
    /// `POST /workflows/:id/save`.
    SaveWorkflow(String),
    /// `GET /mcp/servers` — re-fetched on every entry, never cached.
    LoadMcp,
    /// The MCP verbs. Each is followed by a re-read, because the panel's job is
    /// to show the state that resulted and not the one that was asked for.
    SetMcpEnabled {
        name: String,
        enabled: bool,
    },
    AddMcpServer {
        name: String,
        url: String,
    },
    DeleteMcpServer(String),
    ConnectMcpServer(String),
    RestartMcpServer(String),
    BeginMcpAuth(String),
    ClearMcpAuth(String),
    /// `GET /skills` — the skills TAB's rows (error and sources included), not
    /// the composer's name/description pairs.
    LoadSkillRows,
    /// `GET /models` — the picker's catalog.
    LoadModels,
    /// `GET /model-settings` — both tiers' defaults.
    LoadModelSettings,
    /// `PUT /model-settings` + this session's pin, as one config.
    SaveModel(crate::components::panel::model::ModelConfig),
    // ---- the verbs this client used to refuse ------------------------------
    /// Start a fresh conversation: nothing is posted, but the TRANSPORT has to
    /// forget the session it was reusing or the next send lands in the old one.
    NewConversation,
    /// `/compact` — `POST /sessions/:id/handoff`. The distilled prompt lands in
    /// the COMPOSER of a fresh root: the user reads what was carried over and
    /// edits it before any of it is sent.
    Compact(String),
    /// The composer's `!` sigil: a background shell in this conversation's
    /// workspace. NOT A TURN — nothing is billed and nothing enters the thread.
    RunShell(String),
    /// `POST /sessions/:id/fork` — ⏎ (or `s`) on a turn in the tree.
    Fork {
        session_id: String,
        at_message_id: String,
        exclusive: bool,
        summarize_abandoned: bool,
        editor_text: Option<String>,
    },
    /// `POST /sessions/:id/extract` — `e` in the tree.
    Extract {
        session_id: String,
        picks: Vec<bough_core::schema::requests::PartPick>,
    },
    /// `POST /sessions/:id/move-into` — `m` in the tree.
    MoveInto {
        target_id: String,
        source_id: String,
        picks: Vec<bough_core::schema::requests::PartPick>,
    },
    /// `GET /schedules` — the rail's standing promises.
    LoadSchedules,
    /// `PATCH /schedules/:id {enabled:false}` — the rail's stop on a schedule
    /// DISABLES it: the row leaves, the spec and prompt are kept.
    DisableSchedule(String),
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

/// How long the composer waits before asking the cheap tier what comes next.
const GHOST_DEBOUNCE_MS: i64 = 400;

/// How long the tree's `/` waits before searching every transcript.
const SEARCH_DEBOUNCE_MS: i64 = 180;

/// Below this a conversation is short enough to read whole, and topic headers
/// over it would be chrome captioning four rows.
const SECTION_MIN_TURNS: usize = 8;

/// What a turn contributes to the sections pass: its text, capped — the route
/// reads gists, not transcripts, and the cap is what keeps a long conversation
/// from being sent back in full.
fn turn_gist(m: &Message) -> String {
    let text: String = m
        .parts
        .iter()
        .filter_map(|p| match p {
            bough_core::schema::parts::Part::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let text = if text.is_empty() {
        format!("{:?}", m.role).to_lowercase()
    } else {
        text
    };
    text.chars().take(200).collect()
}

/// The take-back's on-screen affordance. The TS gesture is keymap-only, which
/// makes a three-second window that nothing announces; this row is the window,
/// said out loud, and it expires with it.
pub const TAKE_BACK_HINT: &str = "esc takes that back";

/// The `/commands` this client answers ITSELF, beside the tab-openers. Every
/// one of these is state the reducer owns (or an effect it dispatches), so they
/// come back over the mpsc rather than being refused at the transport.
fn is_client_command(command: Command) -> bool {
    matches!(
        command,
        Command::HelpOpen
            | Command::SessionNew
            | Command::SessionCompact
            | Command::TreeRewind
            | Command::SchedulesShow
    )
}

/// The wire's status string as the selectors' enum. An unknown one is
/// `orphaned`: the rail filters to running/paused, and a status this client
/// cannot name must not be shown as live work.
fn workflow_status(s: &str) -> bough_core::schema::parts::WorkflowStatus {
    use bough_core::schema::parts::WorkflowStatus as W;
    match s {
        "running" => W::Running,
        "paused" => W::Paused,
        "done" => W::Done,
        "error" => W::Error,
        "stopped" => W::Stopped,
        _ => W::Orphaned,
    }
}

/// Every schedule as ONE line — the rail's ⏎ on a timer, and `/schedules`.
///
/// A NOTICE, not a tab. `schedule.*` shipped with no TUI surface at all, so the
/// agent could create a recurring run that fires daily and spends money and the
/// user had no way to see it. This turns "invisible" into "visible", which is
/// the part that matters; it says how to change one, since only the agent can.
/// The TS prints a `MM-DD HH:MM` wall clock here. This crate may not link a
/// calendar (ARCHITECTURE.md §1 keeps it to `bough_core::{schema, errors,
/// types}`), so the same fact is said as the countdown the rail already shows —
/// `next in 4h02m`, or `due` for one that is past its time.
pub fn describe_schedules(rows: &[bough_core::schema::parts::Schedule], now: i64) -> String {
    use crate::store::selectors::{clip, fmt_duration, one_line, plural};
    if rows.is_empty() {
        return "no schedules — ask the agent to add one".to_string();
    }
    let list: Vec<String> = rows
        .iter()
        .map(|r| {
            format!(
                "{}{} {} → next {}",
                if r.enabled { "" } else { "(off) " },
                r.spec,
                clip(
                    &one_line(if r.title.is_empty() {
                        &r.prompt
                    } else {
                        &r.title
                    }),
                    32
                ),
                if r.next_run_at <= now {
                    "due".to_string()
                } else {
                    format!("in {}", fmt_duration(r.next_run_at - now))
                },
            )
        })
        .collect();
    format!(
        "{}: {} — ask the agent to change one",
        plural(rows.len() as i64, "schedule"),
        list.join(" · "),
    )
}

/// `/compact`'s sentences, verbatim.
pub const NOTHING_TO_HAND_OFF: &str = "nothing to hand off yet — this conversation is empty";
pub const DISTILLING: &str = "distilling this conversation into a fresh one…";
/// `^t`, NOT `^f`. The composer-owned chords (^f ^d ^w ^k) are guarded on an
/// empty draft because they are also line-editing keys — and a handoff ALWAYS
/// lands with a draft in the composer, so naming one of those would be naming
/// the one key that cannot work at the one moment the notice is shown.
pub const HANDED_OFF: &str =
    "handed off to a fresh conversation — read the draft, edit it, then send. \
The old thread is untouched: ^t opens the tree";
/// With no goal stated, what "keep going" means.
pub const DEFAULT_HANDOFF_GOAL: &str =
    "continue this work from where it stands, keeping whatever is still needed";
/// The `!` sigil with no conversation to attach a job to. Reached only if the
/// transport did not create one first, which it does.
pub const SHELL_NEEDS_A_CONVERSATION: &str = "! needs a conversation to run in — none is open";
/// ONE shell conversation per workspace, reused — not one per command. It
/// carries a `kind` rather than a title convention, because that is what lets
/// it be found again after a restart.
pub const SHELL_SESSION_TITLE: &str = "shell";

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
    /// The standing promises the rail counts down (`GET /schedules`). Kept
    /// whole, disabled rows included: `/schedules` names those too, and it is
    /// how one is re-enabled.
    schedules: Vec<bough_core::schema::parts::Schedule>,
    /// The runs the rail shows, from the same `GET /workflows` the tab reads.
    workflows: Vec<crate::api::WorkflowSummary>,
    /// `/schedules` is waiting on the listing it asked for. The answer must be
    /// taken NOW rather than read off whatever the rail last cached.
    describe_schedules: bool,
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
    // ---- the cheap-tier cosmetics (row 3.21) -------------------------------
    /// The cheap tier's guess at the next message, shown dim after the input.
    /// Empty is the normal case, and every failure is empty.
    ghost: String,
    /// When the debounced ghost fetch is due, and what it was asked for. A
    /// prediction that appears while you type is a prediction fighting you for
    /// the row, so the timer restarts whenever the conditions change.
    ghost_due: Option<i64>,
    ghost_asked: Option<String>,
    /// Conversations a `sections` pass has been asked for. Marked BEFORE the
    /// reply lands so a second frame does not ask twice.
    sections_asked: HashSet<String>,
    /// When the debounced tree search is due, and the query it will carry.
    search_due: Option<i64>,
    search_asked: String,
    /// The query the running timer belongs to.
    search_pending: String,
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
            ghost: String::new(),
            ghost_due: None,
            ghost_asked: None,
            sections_asked: HashSet::new(),
            search_due: None,
            search_asked: String::new(),
            search_pending: String::new(),
            panel: PanelHost::default(),
            help_open: false,
            help_off: 0,
            jobs: Vec::new(),
            schedules: Vec::new(),
            workflows: Vec::new(),
            describe_schedules: false,
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
        // Same adaptation as the rows above, for the same reason: the run list
        // is declared in `api.rs` (what the wire carries) and in `state.rs`
        // (what the selectors take). The field list is written out, so a
        // divergence is a compile error here.
        let runs: Vec<crate::store::state::WorkflowSummary> = self
            .workflows
            .iter()
            .map(|w| crate::store::state::WorkflowSummary {
                id: w.id.clone(),
                name: w.name.clone(),
                description: w.description.clone(),
                status: workflow_status(&w.status),
                current_phase: w.current_phase.clone(),
                // Not on the summary wire shape, and the rail reads none of
                // them: a row is a title, a phase and a progress bar.
                phases: Vec::new(),
                agents: crate::store::state::WorkflowAgentCounts {
                    total: w.agents.total as i64,
                    done: w.agents.done as i64,
                    cached: w.agents.cached as i64,
                    running: w.agents.running as i64,
                    queued: w.agents.queued as i64,
                    failed: w.agents.failed as i64,
                },
                result: None,
                error: None,
                resume_of: None,
                created_at: w.created_at,
                finished_at: w.finished_at,
                script_file: String::new(),
            })
            .collect();
        live_units(&self.jobs, &subagents, &runs, &self.schedules, self.now_ms)
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

    /// The terminal-window / multiplexer-tab title for the state on screen.
    ///
    /// The conversation's own title, and whether its turn is running — the two
    /// facts a tab bar can carry. `main.tsx` computes exactly this and pushes it
    /// only when it CHANGES; the push itself is the loop's (`run_loop`).
    pub fn tab_title(&self, spinner_frame: usize) -> String {
        let title = self.session_id.as_ref().and_then(|id| {
            self.panel
                .sessions
                .iter()
                .find(|row| &row.session.id == id)
                .map(|row| row.session.title.clone())
        });
        let status = match &self.turn {
            Some(turn) if !turn.ended => Some(crate::term::TitleStatus::Running),
            Some(_) => Some(crate::term::TitleStatus::Complete),
            None => None,
        };
        crate::term::bough_title(title.as_deref(), status, spinner_frame)
    }

    /// The spinner frame the title is drawn at — the loop's own tick, which
    /// runs at `SPINNER_MS` (the TS title spinner's 120ms exactly).
    pub fn spinner_frame(&self) -> usize {
        self.tick as usize
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
                if self.poll_tick.is_multiple_of(POLL_TICKS) && self.session_id.is_some() {
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
            Action::JobOutput {
                id,
                output,
                job,
                error,
            } => {
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
                self.notice =
                    Some(crate::store::lifecycle::take_back_notice(self.busy()).to_string());
                self.cursor = text.chars().count();
                self.draft = text;
                self.scroll_off = 0;
                self.last_send_at = None;
            }
            Action::Draft(text) => {
                self.cursor = text.chars().count();
                self.draft = text;
                self.scroll_off = 0;
            }
            Action::Schedules(rows) => {
                self.schedules = rows;
                if std::mem::take(&mut self.describe_schedules) {
                    self.notice = Some(describe_schedules(&self.schedules, self.now_ms));
                }
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
                // …and the rest of the rail's feed. Both are then kept fresh by
                // events rather than by a poll (`reduce_event`), which is the
                // TS's policy and the reason neither has a timer.
                self.transport.effect(Effect::LoadWorkflows);
                self.transport.effect(Effect::LoadSchedules);
            }
            Action::Thread(thread) => {
                self.thread = thread;
                self.streaming.clear();
                // A switch lands at the live tail, like every arrival.
                self.scroll_off = 0;
            }
            Action::Sessions(sessions) => self.panel.set_sessions(sessions),
            Action::Ghost(ghost) => {
                // Late is the same as never for a prediction: if the composer
                // is no longer empty and idle, the row it would paint on is
                // the row the user is typing into.
                if self.ghost_wanted().is_some() {
                    self.ghost = ghost;
                }
            }
            Action::Sections {
                session_id,
                sections,
            } => {
                self.panel.sections.insert(session_id, sections);
            }
            Action::SearchHits {
                q,
                sessions,
                messages,
            } => {
                self.panel.set_search_hits(&q, sessions, messages);
            }
            Action::Changes(set) => self.panel.set_changes(set),
            Action::Theme(state) => self.panel.set_theme(state),
            Action::Workflows(runs) => {
                // The rail and the tab read ONE list: two fetches of the same
                // shape is how a row comes to be live in one surface and
                // finished in the other.
                self.workflows = runs.clone();
                self.panel.set_workflows(runs);
            }
            Action::Workflow(detail) => {
                // A failed fetch drops back to the list: a detail level with no
                // detail is a header full of zeroes over a run that may not
                // exist.
                if detail.is_none() && self.panel.wf_level > 0 {
                    self.panel.wf_level = 0;
                }
                self.panel.set_workflow_detail(detail);
            }
            Action::Mcp(status) => self.panel.set_mcp(status),
            Action::SkillRows {
                skills,
                sources,
                note,
            } => self.panel.set_skills(skills, sources, note),
            Action::Models(models) => self.panel.set_models(models),
            Action::ModelSettings(settings) => {
                // The session's OWN pin is what this screen runs on; the
                // settings answer what a NEW conversation would.
                let open = self
                    .session_id
                    .as_ref()
                    .and_then(|id| self.panel.sessions.iter().find(|row| &row.session.id == id));
                let cfg = crate::components::panel::model::ModelConfig {
                    default_model: settings.default_model,
                    session_model: open.and_then(|row| row.session.model.clone()),
                    cheap_model: settings.cheap_model,
                    default_effort: settings
                        .default_effort
                        .map(crate::components::panel::model::EffortChoice::Level)
                        .unwrap_or(crate::components::panel::model::EffortChoice::Default),
                    session_effort: crate::components::panel::model::as_effort_choice(
                        open.and_then(|row| row.session.effort.as_deref()),
                    ),
                };
                self.panel.set_model_config(cfg);
            }
            Action::Run(command, arg) => self.run_client_command(command, &arg),
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
        self.cosmetics(now_ms);
    }

    // ---- the cheap-tier cosmetics (row 3.21) -------------------------------
    //
    // Ghost text, topic headers and the tree's full-text search. All three are
    // cheap-tier answers, all three are debounced, and EVERY failure is silence
    // — a feature whose whole value is that you can ignore it must never put a
    // banner on the screen. Driven off the loop's own tick rather than three
    // timers: the tick already arrives while the screen is idle.

    /// The conversation a prediction would be FOR, or None when one would be
    /// noise: mid-turn (the conversation is still moving), mid-typing (it would
    /// fight the user for the row), or on an empty conversation.
    fn ghost_wanted(&self) -> Option<&str> {
        if self.busy() || !self.draft.is_empty() || self.thread.is_empty() {
            return None;
        }
        self.session_id.as_deref()
    }

    fn cosmetics(&mut self, now_ms: i64) {
        self.mirror_thread();
        self.tick_ghost(now_ms);
        self.tick_sections();
        self.tick_search(now_ms);
    }

    /// The open conversation's turns, where the tree reads them from. Without
    /// this the topic headers and the searched-turn marks would have nothing to
    /// hang on — the tree's own thread map is filled per session.
    fn mirror_thread(&mut self) {
        let Some(id) = self.session_id.clone() else {
            return;
        };
        if self.panel.threads.get(&id).map(Vec::len) != Some(self.thread.len()) {
            self.panel.threads.insert(id, self.thread.clone());
        }
    }

    fn tick_ghost(&mut self, now_ms: i64) {
        let Some(id) = self.ghost_wanted().map(str::to_string) else {
            // The conditions stopped holding: drop the prediction AND the
            // in-flight ask, so typing then stopping asks again.
            self.ghost.clear();
            self.ghost_due = None;
            self.ghost_asked = None;
            return;
        };
        if self.ghost_asked.as_deref() == Some(id.as_str()) {
            return;
        }
        match self.ghost_due {
            None => self.ghost_due = Some(now_ms + GHOST_DEBOUNCE_MS),
            Some(due) if now_ms >= due => {
                self.ghost_due = None;
                self.ghost_asked = Some(id.clone());
                self.transport.effect(Effect::GhostText(id));
            }
            Some(_) => {}
        }
    }

    /// One pass per conversation, and only for one long enough to need it: the
    /// route is a cheap-tier LLM read of every turn gist, and it is stateless,
    /// so the answer is cached rather than re-asked as the cursor moves.
    fn tick_sections(&mut self) {
        if !self.panel.open() || self.panel.tab() != crate::keys::PanelTab::Tree {
            return;
        }
        let want: Vec<(String, Vec<String>)> = self
            .panel
            .expanded
            .iter()
            .filter(|id| !self.sections_asked.contains(*id))
            .filter_map(|id| {
                let thread = self.panel.threads.get(id)?;
                (thread.len() >= SECTION_MIN_TURNS)
                    .then(|| (id.clone(), thread.iter().map(turn_gist).collect()))
            })
            .collect();
        for (session_id, gists) in want {
            self.sections_asked.insert(session_id.clone());
            self.transport
                .effect(Effect::Sections { session_id, gists });
        }
    }

    /// The tree's `/` is a full-text search of every message — which is what
    /// the keymap has always said it is, and what it never did.
    fn tick_search(&mut self, now_ms: i64) {
        let q = if self.panel.open() && self.panel.tab() == crate::keys::PanelTab::Tree {
            self.panel.filter.trim().to_string()
        } else {
            String::new()
        };
        // One character matches everything; FTS over every transcript is not
        // free, and the title filter alone already answers that keystroke.
        if q.chars().count() < 2 {
            self.search_due = None;
            self.search_asked.clear();
            return;
        }
        // The timer is restarted by a CHANGED query, not by the tick that
        // happens to be checking it — a countdown reset every frame never
        // reaches zero.
        if q != self.search_pending {
            self.search_pending = q;
            self.search_due = Some(now_ms + SEARCH_DEBOUNCE_MS);
            return;
        }
        if q == self.search_asked {
            return;
        }
        if self.search_due.is_some_and(|due| now_ms >= due) {
            self.search_due = None;
            self.search_asked = q.clone();
            self.transport.effect(Effect::SearchSessions(q));
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
        let at = Point {
            x: m.column as i64 + 1,
            y: m.row as i64 + 1,
        };
        match m.kind {
            MouseEventKind::ScrollUp => self.scroll_by(WHEEL_ROWS as isize),
            MouseEventKind::ScrollDown => self.scroll_by(-(WHEEL_ROWS as isize)),
            MouseEventKind::Down(MouseButton::Left) => {
                self.sel = Some(Selection {
                    anchor: at,
                    focus: at,
                });
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
        let area = Rect {
            x: 0,
            y: 0,
            width: self.cols.max(20),
            height: self.rows.max(8),
        };
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
        let Some(here) = col.checked_sub(offset) else {
            return;
        };
        if let Some(url) = url_at(&content, here) {
            self.open_link(&url);
            return;
        }
        // A long address — the mcp tab's authorization link is the case that
        // matters — is wrapped over several rows, and no single one of them is a
        // URL. `url_across` rejoins them, so a click anywhere in it opens the
        // whole thing rather than a fragment.
        let contents: Vec<String> = painted.iter().map(|r| row_content(r).0).collect();
        if let Some(url) = usize::try_from(at.y - 1)
            .ok()
            .and_then(|y| url_across(&contents, y, here))
        {
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
            let candidates: Vec<Candidate> = self
                .files
                .iter()
                .map(|name| Candidate::file(name.clone()))
                .collect();
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
        candidates.extend(
            self.skills
                .iter()
                .map(|(name, desc)| Candidate::skill(name, desc)),
        );
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
        let Some(trigger) = self.trigger() else {
            return;
        };
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
        let Some(trigger) = self.trigger() else {
            return;
        };
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
        composer_height(
            &self.draft,
            &self.ghost,
            self.busy(),
            cols,
            composer_rows,
            0,
        ) as u16
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
            // While the `/` buffer has the keyboard, bare letters are text —
            // the keymap's own guard, and the reason `j`/`k` do not walk the
            // list mid-query.
            panel_filtering: self.panel.filtering,
            ..Default::default()
        };
        lookup(&ctx, &crate::keys::chord_of(&input, flags))
    }

    /// A `/command` this client answers itself (a tab, the overlay, the
    /// session verbs). `arg` is empty for every command but `/compact`.
    fn run_client_command(&mut self, command: Command, arg: &str) {
        match command {
            Command::HelpOpen => {
                self.help_open = true;
                self.help_off = 0;
            }
            Command::HelpClose => self.help_open = false,
            // The DRAFT goes too: it was written for the conversation being
            // left, and carrying it into a fresh one is how you send the wrong
            // thing to the wrong thread.
            Command::SessionNew => {
                self.clear_draft();
                self.scroll_off = 0;
                self.session_id = None;
                self.panel.current_id = None;
                self.thread.clear();
                self.streaming.clear();
                self.turn = None;
                self.activity = None;
                self.notice = None;
                self.last_send_at = None;
                // Another conversation's shells and holds pinned under this
                // composer would be a claim about work this screen is not doing.
                self.jobs.clear();
                self.rail_sel = None;
                self.rail_armed = None;
                self.job = None;
                self.ask = None;
                self.ask_typed.clear();
                self.help_open = false;
                self.panel.state.open = false;
                // The transport is reusing a session id of its own; without
                // this the next send would land back in the old conversation.
                self.transport.effect(Effect::NewConversation);
            }
            // The old conversation is neither mutated nor inherited, so there
            // is nothing to confirm — but it does call the model, which is why
            // this announces itself before it starts.
            Command::SessionCompact => {
                if self.session_id.is_none() {
                    return;
                }
                if self.thread.is_empty() {
                    self.notice = Some(NOTHING_TO_HAND_OFF.to_string());
                    return;
                }
                // The goal steers what survives. With none stated the
                // instruction has to say what "keep going" means, or the
                // summarizer is left guessing which of two finished threads of
                // work the next message is about.
                let goal = arg.trim();
                let stated = if goal.is_empty() {
                    DEFAULT_HANDOFF_GOAL
                } else {
                    goal
                };
                self.notice = Some(DISTILLING.to_string());
                self.transport.effect(Effect::Compact(stated.to_string()));
            }
            // The rows the rail already counts down, said in full. Re-read
            // first: the answer must be taken NOW, not off a cached list.
            Command::SchedulesShow => {
                self.describe_schedules = true;
                self.transport.effect(Effect::LoadSchedules);
            }
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
                // ---- row 3.20 ---------------------------------------------
                HostRequest::LoadWorkflows => self.transport.effect(Effect::LoadWorkflows),
                HostRequest::LoadWorkflow(id) => self.transport.effect(Effect::LoadWorkflow(id)),
                HostRequest::SteerWorkflow { id, action } => {
                    self.transport.effect(Effect::SteerWorkflow { id, action })
                }
                HostRequest::SaveWorkflow(id) => self.transport.effect(Effect::SaveWorkflow(id)),
                HostRequest::OpenAgentSession(id) => self.transport.effect(Effect::OpenSession(id)),
                HostRequest::LoadMcp => self.transport.effect(Effect::LoadMcp),
                HostRequest::SetMcpEnabled { name, enabled } => self
                    .transport
                    .effect(Effect::SetMcpEnabled { name, enabled }),
                HostRequest::AddMcpServer { name, url } => {
                    self.transport.effect(Effect::AddMcpServer { name, url })
                }
                HostRequest::DeleteMcpServer(name) => {
                    self.transport.effect(Effect::DeleteMcpServer(name))
                }
                HostRequest::ConnectMcpServer(name) => {
                    self.transport.effect(Effect::ConnectMcpServer(name))
                }
                HostRequest::RestartMcpServer(name) => {
                    self.transport.effect(Effect::RestartMcpServer(name))
                }
                HostRequest::BeginMcpAuth(name) => {
                    self.transport.effect(Effect::BeginMcpAuth(name))
                }
                HostRequest::ClearMcpAuth(name) => {
                    self.transport.effect(Effect::ClearMcpAuth(name))
                }
                HostRequest::LoadSkillRows => self.transport.effect(Effect::LoadSkillRows),
                HostRequest::LoadModels => self.transport.effect(Effect::LoadModels),
                HostRequest::LoadModelSettings => self.transport.effect(Effect::LoadModelSettings),
                HostRequest::SaveModel(cfg) => self.transport.effect(Effect::SaveModel(cfg)),
                HostRequest::Fork {
                    session_id,
                    at_message_id,
                    exclusive,
                    summarize_abandoned,
                    editor_text,
                } => self.transport.effect(Effect::Fork {
                    session_id,
                    at_message_id,
                    exclusive,
                    summarize_abandoned,
                    editor_text,
                }),
                HostRequest::Extract { session_id, picks } => {
                    self.transport.effect(Effect::Extract { session_id, picks })
                }
                HostRequest::MoveInto {
                    target_id,
                    source_id,
                    picks,
                } => self.transport.effect(Effect::MoveInto {
                    target_id,
                    source_id,
                    picks,
                }),
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
            // While the panel's `/` buffer is open, an unbound printable key
            // IS the query — the one place in the panel where text is text.
            if self.panel.filtering {
                if let KeyCode::Char(c) = k.code {
                    if !c.is_control() && !k.modifiers.contains(KeyModifiers::ALT) {
                        self.panel.type_filter(c);
                        return true;
                    }
                }
            }
            // In a surface that owns the keyboard, an unbound key is eaten
            // rather than typed into a composer nobody can see.
            return self.help_open
                || self.panel.open()
                || self.job.is_some()
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
                // ⇥ with no popup TAKES the prediction: it replaces the draft
                // and is gone. Nothing happens when there is none — the key
                // falls through rather than eating itself.
                Command::GhostAccept => {
                    if self.ghost.is_empty() {
                        return false;
                    }
                    self.draft = std::mem::take(&mut self.ghost);
                    self.cursor = self.draft.chars().count();
                    true
                }
                _ => false,
            },
        }
    }

    // ---- the rail's keys ---------------------------------------------------

    fn on_rail_command(&mut self, command: Command, _digit: Option<usize>) {
        let units = self.units();
        let at = self
            .rail_sel
            .unwrap_or(0)
            .min(units.len().saturating_sub(1));
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
                        self.transport
                            .effect(Effect::LoadJobOutput(unit.id.clone()));
                    }
                    LiveUnitKind::Subagent => {
                        self.transport.effect(Effect::OpenSession(unit.id.clone()))
                    }
                    // A run's surface is the workflows tab, drilled in.
                    LiveUnitKind::Workflow => {
                        let id = unit.id.clone();
                        self.rail_sel = None;
                        let requests = self.panel.open_run(&id);
                        self.serve(requests);
                    }
                    // A schedule has no session to open and no buffer to show —
                    // ⏎ says what it is and when it next fires. The cursor
                    // stays on the rail: you glanced, you did not leave.
                    LiveUnitKind::Schedule => {
                        self.notice = Some(describe_schedules(&self.schedules, self.now_ms));
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
                    LiveUnitKind::Shell => self.transport.effect(Effect::KillJob(unit.id.clone())),
                    LiveUnitKind::Subagent => {
                        self.transport.effect(Effect::StopSession(unit.id.clone()))
                    }
                    LiveUnitKind::Workflow => self.transport.effect(Effect::SteerWorkflow {
                        id: unit.id.clone(),
                        action: crate::components::panel::host::WorkflowAction::Stop,
                    }),
                    // Stopping a schedule is DISABLING it, not deleting: the
                    // row leaves the rail, the schedule keeps its spec and its
                    // prompt, and the agent can turn it back on.
                    LiveUnitKind::Schedule => self
                        .transport
                        .effect(Effect::DisableSchedule(unit.id.clone())),
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
        let Some(view) = self.job.as_mut() else {
            return;
        };
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
                let Some(d) = digit.filter(|d| *d > 0) else {
                    return;
                };
                let Some(option) = ask.options.as_ref().and_then(|o| o.get(d - 1)).cloned() else {
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
                    self.notice =
                        Some("^c again to quit — subagents and workflows keep running".to_string());
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
            (KeyCode::Right, _) => self.cursor = (self.cursor + 1).min(self.draft.chars().count()),
            (KeyCode::PageUp, _) => self.scroll_by(self.page() as isize),
            (KeyCode::PageDown, _) => self.scroll_by(-(self.page() as isize)),
            (KeyCode::Char(c), false)
                // stripCtl-lite: whole control chars never reach the draft;
                // meta chords are commands, never text (inkKey: Option = meta).
                if !c.is_control() && !k.modifiers.contains(KeyModifiers::ALT) => {
                    self.insert_char(c);
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
        // esc esc with NOTHING typed and NOTHING running: the tree, opened on
        // your last turn.
        //
        // The gesture already meant "undo the thing I am in the middle of" — it
        // cleared a draft, it stopped a turn. With an empty composer and an
        // idle session it meant "dismiss a notice", which is nothing at all,
        // while the actual undo one reaches for at that moment — go back a
        // message and say it differently — was four keypresses into a panel tab.
        if self
            .last_esc_at
            .is_some_and(|at| now_ms - at < DOUBLE_ESC_MS)
        {
            self.last_esc_at = None;
            self.run_client_command(Command::TreeRewind, "");
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
        // `!command` IS THE USER'S OWN SHELL, not a message. Every comparable
        // harness honours the sigil; bough printed "! is not a shell — this
        // goes to the model" and made the user ask the agent to run `ls`. It is
        // NOT A TURN: nothing is billed, nothing enters the thread, and the job
        // lands in the rail where its output is already readable on ⏎.
        if text.starts_with('!') && !text[1..].trim().is_empty() {
            let command = text[1..].trim().to_string();
            self.clear_draft();
            self.scroll_off = 0;
            // (The TS also pushes the line, sigil and all, onto the ↑ history
            // so re-running is ↑⏎. This client has no history ring yet, so
            // there is nothing to push it onto.)
            self.transport.effect(Effect::RunShell(command));
            return;
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
                suggestion
                    .map(|s| format!(" — did you mean /{s}?"))
                    .unwrap_or_default(),
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
        // Schedules have no events of their own. The agent edits one during a
        // turn (so the turn finishing is when the edit is final), and a FIRE
        // announces itself as the fired root's `session.created` — between
        // them, every change to `next_run_at` has a signal, so the rail's
        // countdown needs no poll of its own.
        if matches!(
            event.r#type,
            EventType::TurnFinished | EventType::SessionCreated
        ) {
            self.transport.effect(Effect::LoadSchedules);
        }

        // (Ahead of the per-session filter: a schedule FIRES into a session
        // that is not this one, and that event is exactly the news.)
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
                    self.turn = Some(TurnClock {
                        started_at: event.ts,
                        ended: false,
                    });
                }
                // The server's copy of a message we sent supersedes the
                // optimistic local echo (the TS store reconciles by id via the
                // snapshot merge; v1 matches the echo by text).
                if msg.role == Role::User {
                    let text = first_text(&msg);
                    if let Some(pos) = self
                        .thread
                        .iter()
                        .position(|m| m.id.starts_with("local-") && first_text(m) == text)
                    {
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
                    self.streaming
                        .entry(d.message_id)
                        .or_default()
                        .push_str(&d.delta);
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
            // A run's state moved. The event says "re-read" and never carries
            // the row: one shape (`GET /workflows`) decides what a run is, so an
            // event that arrives out of order cannot invent one. Only while the
            // tab is open — a closed panel polling a fan-out is a background
            // request nobody asked for.
            EventType::WorkflowUpdated | EventType::WorkflowAgent => {
                // The RAIL shows runs too, and it is visible with the panel
                // shut — so the list is re-read either way. Only the open run's
                // detail is gated on the tab, which is the request nobody asked
                // for when the panel is closed.
                self.transport.effect(Effect::LoadWorkflows);
                if self.panel.open() && self.panel.tab() == crate::keys::PanelTab::Workflows {
                    if let Some(id) = self
                        .panel
                        .run_detail
                        .as_ref()
                        .map(|d| d.workflow.id.clone())
                    {
                        self.transport.effect(Effect::LoadWorkflow(id));
                    }
                }
            }
            // The narrator line, which the run header prints as its `▸` row.
            // Kept for the OPEN run only: a line from another run under this
            // run's header is a sentence about work you are not looking at.
            EventType::WorkflowLog => {
                let Ok(d) = serde_json::from_value::<bough_core::schema::events::WorkflowLogData>(
                    event.data.clone(),
                ) else {
                    return;
                };
                if self
                    .panel
                    .run_detail
                    .as_ref()
                    .map(|r| r.workflow.id.as_str())
                    == Some(d.run_id.as_str())
                {
                    self.panel.last_log = Some(d.line);
                }
            }
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
        // Derived ONCE, here, because the props borrow them: two derivations of
        // "which rows are visible" is how a digit comes to select a row nobody
        // can see (the host's `pick_target` walks these same functions).
        let skills = self
            .panel
            .skills
            .as_ref()
            .map(|_| self.panel.filtered_skills());
        let entries = self.panel.model_entries();
        let body = match self.panel.tab() {
            crate::keys::PanelTab::Tree => {
                PanelBody::Tree(crate::components::panel::tree::TreeProps {
                    rows: &rows,
                    selected: self.panel.sel,
                    height: panel_body_rows((area.height as usize).saturating_sub(2)),
                    workspace: self.options.workspace.as_deref(),
                    cols: Some((area.width as usize).saturating_sub(4).max(20)),
                    message: self.panel.message.as_deref(),
                    // The `/` buffer echoes as its own row while it has the
                    // keyboard AND after: a narrowed list must say what
                    // narrowed it.
                    filter: (!self.panel.filter.is_empty()).then_some(self.panel.filter.as_str()),
                    filtering: self.panel.filtering,
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
                    hint: self
                        .session_id
                        .as_ref()
                        .map(|_| crate::components::panel::changes::NOT_A_REPO_HINT),
                })
            }
            crate::keys::PanelTab::Theme => PanelBody::Theme(self.panel.theme.as_ref()),
            crate::keys::PanelTab::Workflows => {
                PanelBody::Workflows(crate::components::panel::workflows::WorkflowsProps {
                    runs: &self.panel.runs,
                    sel: self.panel.sel,
                    level: self.panel.wf_level,
                    detail: self.panel.run_detail.as_ref(),
                    phase_sel: self.panel.phase_sel,
                    agent_sel: self.panel.agent_sel,
                    scroll: self.panel.wf_scroll,
                    filter: self.panel.wf_filter(),
                    prompt_open: self.panel.prompt_open,
                    rows: panel_body_rows((area.height as usize).saturating_sub(2)),
                    cols: (area.width as usize).saturating_sub(4).max(20),
                    last_log: self.panel.last_log.as_deref(),
                    now: self.now_ms,
                })
            }
            crate::keys::PanelTab::Mcp => {
                PanelBody::Mcp(crate::components::panel::mcp::McpTabProps {
                    status: self.panel.mcp.as_ref(),
                    selected: self.panel.sel,
                    message: self.panel.message.as_deref(),
                    rows: panel_body_rows((area.height as usize).saturating_sub(2)),
                    cols: (area.width as usize).saturating_sub(4).max(20),
                    entry: self.panel.mcp_entry.as_deref(),
                })
            }
            crate::keys::PanelTab::Skills => {
                PanelBody::Skills(crate::components::panel::skills::SkillsTabProps {
                    // `filtered` is a temporary, so it is built into the field
                    // the props borrow — the list the cursor addresses and the
                    // list painted must be the SAME derivation.
                    skills: skills.as_deref(),
                    rows: panel_body_rows((area.height as usize).saturating_sub(2)),
                    cols: (area.width as usize).saturating_sub(4).max(20),
                    selected: self.panel.sel,
                    note: self.panel.skills_note.as_deref(),
                    sources: &self.panel.skill_sources,
                    filter: &self.panel.filter,
                    filtering: self.panel.filtering,
                })
            }
            crate::keys::PanelTab::Model => {
                PanelBody::Model(crate::components::panel::model::ModelPickerProps {
                    cols: (area.width as usize).saturating_sub(4).max(20),
                    cfg: &self.panel.model_cfg,
                    entries: &entries,
                    selected: self.panel.sel,
                    rows: panel_body_rows((area.height as usize).saturating_sub(2)),
                    message: self.panel.message.as_deref(),
                    filters: &self.panel.model_filters,
                    focused: self.panel.model_focus,
                })
            }
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
            .map(|w| {
                w.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or(w)
                    .to_string()
            })
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
        let growing = Rect {
            x: area.x,
            y: area.y + 1,
            width: cols,
            height: chat_h,
        };
        // The growing region is the transcript OR the panel — the panel
        // DISPLACES it rather than floating over it, which is what makes
        // "there is exactly one place that is not the chat" true on screen.
        if let Some(view) = &self.job {
            // The open job takes the growing region: it is a reading surface,
            // and it returns to the rail it was opened from.
            let sub = job_sub_lines(view.job.as_ref(), &view.id, cols as usize, chat_h as usize);
            render_job_output(
                &JobOutputProps {
                    id: &view.id,
                    job: view.job.as_ref(),
                    output: &view.output,
                    scroll: view.scroll.min(
                        view.output
                            .lines()
                            .count()
                            .saturating_sub(job_body_rows(chat_h as usize, sub.len())),
                    ),
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
                Rect {
                    x: area.x,
                    y: area.y + 1 + chat_h,
                    width: cols,
                    height: popup_h,
                },
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
                Rect {
                    x: area.x,
                    y: area.y + 1 + chat_h + popup_h,
                    width: cols,
                    height: rail_h,
                },
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
                &AskCardProps {
                    lines: &lines,
                    options: &options,
                    typed: &self.ask_typed,
                },
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
                    // Suppressed while the popup is up: two dim suggestions
                    // competing for one row, and ⇥ belongs to the popup then.
                    ghost: if self.trigger().is_some() {
                        ""
                    } else {
                        &self.ghost
                    },
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
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
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
        parts.push(format!(
            "not in this change set: {}",
            outcome.skipped.join(", ")
        ));
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
/// The terminal's timers, driven by the loop's own tick instead of by the
/// runtime.
///
/// `Term`'s callbacks are `Fn()` and not `Send` (they close over the writer),
/// so they cannot be handed to `tokio::spawn`. They do not need to be: the loop
/// already wakes at `SPINNER_MS`, and both users of this — the 5s progress
/// keep-alive and the 4s error-progress clear — are far coarser than that.
#[derive(Default)]
struct LoopTimers {
    next: Cell<u64>,
    /// The last tick's clock, so a timer added between ticks is scheduled
    /// against a time this object has actually seen.
    now: Cell<i64>,
    /// `(handle, due_ms, repeat_every, callback)`.
    entries: RefCell<Vec<(u64, i64, Option<u64>, Box<dyn Fn()>)>>,
}

impl LoopTimers {
    fn add(&self, f: Box<dyn Fn()>, ms: u64, repeat: bool) -> u64 {
        let h = self.next.get() + 1;
        self.next.set(h);
        // Scheduled against the LAST tick, not a fresh clock read: `fire` is
        // the only place time is read, so a timer added between ticks is due
        // one full period after the tick that follows it — never immediately.
        let due = self.now.get() + ms as i64;
        self.entries
            .borrow_mut()
            .push((h, due, repeat.then_some(ms), f));
        h
    }
    fn remove(&self, handle: u64) {
        self.entries.borrow_mut().retain(|(h, ..)| *h != handle);
    }
    /// Run everything due at `now`. Callbacks run OUTSIDE the borrow: one that
    /// clears or adds a timer would otherwise panic on re-entry.
    fn fire(&self, now: i64) {
        self.now.set(now);
        let due: Vec<u64> = self
            .entries
            .borrow()
            .iter()
            .filter(|(_, at, ..)| now >= *at)
            .map(|(h, ..)| *h)
            .collect();
        for h in due {
            self.call(h, now);
        }
    }
    /// Take the entry out, run it, and put a REPEATING one back at its next
    /// due time. A one-shot stays out — including one whose callback cleared
    /// itself, which is what the error-progress timer does.
    fn call(&self, handle: u64, now: i64) {
        let taken = {
            let mut entries = self.entries.borrow_mut();
            entries
                .iter()
                .position(|(h, ..)| *h == handle)
                .map(|i| entries.remove(i))
        };
        let Some((h, _, period, f)) = taken else {
            return;
        };
        f();
        if let Some(ms) = period {
            // Only if the callback did not clear it while it ran.
            let mut entries = self.entries.borrow_mut();
            if !entries.iter().any(|(x, ..)| *x == h) {
                entries.push((h, now + ms as i64, Some(ms), f));
            }
        }
    }
}

impl crate::term::TermTimers for LoopTimers {
    fn set_interval(&self, f: Box<dyn Fn()>, ms: u64) -> u64 {
        self.add(f, ms, true)
    }
    fn clear_interval(&self, handle: u64) {
        self.remove(handle);
    }
    fn set_timeout(&self, f: Box<dyn Fn()>, ms: u64) -> u64 {
        self.add(f, ms, false)
    }
    fn clear_timeout(&self, handle: u64) {
        self.remove(handle);
    }
}

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
    // Focus reporting: `notify_desktop` is silent while focused, and it can
    // only know that if the terminal says so.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableFocusChange);
    // The terminal's own chrome: window/tab title, taskbar progress, the
    // desktop banner. Every one is capability-gated inside `Term` and every one
    // degrades to nothing where the terminal cannot do it (term.rs).
    let timers = std::rc::Rc::new(LoopTimers::default());
    let term = crate::term::create_term(crate::term::TermOptions {
        caps: crate::term::term_caps(&std::env::vars().collect()),
        write: std::rc::Rc::new(|seq: &str| {
            use std::io::Write;
            let mut out = std::io::stdout();
            let _ = out.write_all(seq.as_bytes());
            let _ = out.flush();
        }),
        // OSC 0 names the pane; tmux's and zellij's own CLIs name the WINDOW
        // and the TAB, which no escape sequence can reach. Detached, output
        // ignored: a multiplexer that is not there is not an error.
        rename_tmux_window: Some(std::rc::Rc::new(|title: &str| {
            let _ = std::process::Command::new("tmux")
                .args(["rename-window", "--", title])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        })),
        rename_zellij_tab: Some(std::rc::Rc::new(|title: &str| {
            let _ = std::process::Command::new("zellij")
                .args(["action", "rename-tab", title])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        })),
        timers: timers.clone(),
    });
    let mut tab_title = String::new();
    let mut was_busy = false;
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
        // Focus is the terminal's own report, not a keypress: it decides
        // whether a finished turn is worth a desktop banner.
        match &action {
            Action::Term(TermEvent::FocusGained) => term.set_focused(true),
            Action::Term(TermEvent::FocusLost) => term.set_focused(false),
            _ => {}
        }
        app.apply(action, now);
        // The progress keep-alive rides the loop's own tick (Ghostty expires a
        // stale progress after ~15s, so `Term` re-asserts every 5s).
        timers.fire(now);
        // The chrome, pushed only when it CHANGES: a title written every frame
        // makes a tab bar flicker, and a terminal that renames a tmux window
        // per keystroke spawns a process per keystroke.
        let title = app.tab_title(app.spinner_frame());
        if title != tab_title {
            tab_title = title;
            term.set_title(&tab_title);
        }
        if app.busy() != was_busy {
            was_busy = app.busy();
            if was_busy {
                term.progress_start();
            } else {
                term.progress_end(false);
                // ONLY while unfocused — `Term` enforces that itself, so the
                // call is unconditional here and silent when you are looking.
                term.notify_desktop("bough finished a turn");
            }
        }
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
    // Every sticky thing this process set on the terminal: the progress bar and
    // any tab tint. Left behind, they outlive the program in the user's tab.
    term.cleanup();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableFocusChange);
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
                        let _ = tx.send(Action::DirEntries {
                            prefix,
                            entries: list.entries,
                        });
                    }
                });
            }
            Effect::LoadSkills => {
                tokio::spawn(async move {
                    // No skills, no `/` rows — never a modal.
                    if let Ok(list) = api.list_skills().await {
                        let _ = tx.send(Action::Skills(
                            list.skills
                                .into_iter()
                                .map(|s| (s.name, s.description))
                                .collect(),
                        ));
                    }
                });
            }
            Effect::Run(command, arg) => {
                // The surfaces this client owns answer themselves, back over
                // the same mpsc; the rest are honest about what they cannot do
                // — and, crucially, the command still never reaches the model.
                if tab_for_command(command).is_some() || is_client_command(command) {
                    let _ = self.tx.send(Action::Run(command, arg));
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
                    let Some(sid) = session.lock().expect("session lock").clone() else {
                        return;
                    };
                    // A poll that fails is a beat with no news, never a modal:
                    // the rail keeps the rows it had and the next tick retries.
                    if let Ok(list) = api.list_jobs(&sid).await {
                        let _ =
                            tx.send(Action::Jobs(list.jobs.into_iter().map(|r| r.job).collect()));
                    }
                });
            }
            Effect::LoadJobOutput(job_id) => {
                tokio::spawn(async move {
                    let Some(sid) = session.lock().expect("session lock").clone() else {
                        return;
                    };
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
                    let Some(sid) = session.lock().expect("session lock").clone() else {
                        return;
                    };
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
                    let Some(sid) = session.lock().expect("session lock").clone() else {
                        return;
                    };
                    if let Ok(asks) = api.list_questions(Some(&sid)).await {
                        let _ = tx.send(Action::Asks(asks));
                    }
                });
            }
            Effect::AnswerAsk {
                session_id,
                id,
                answer,
            } => {
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
                    let Some(sid) = session.lock().expect("session lock").clone() else {
                        return;
                    };
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
            // The three cheap-tier cosmetics (row 3.21). Every failure is
            // SILENCE — not a notice: a prediction, a topic header and a search
            // that could not answer are all things the screen is fine without,
            // and a banner for one would be worse than the missing feature.
            Effect::GhostText(id) => {
                tokio::spawn(async move {
                    if let Ok(r) = api.ghost_text(&id, "").await {
                        let _ = tx.send(Action::Ghost(r.ghost.unwrap_or_default()));
                    }
                });
            }
            Effect::Sections { session_id, gists } => {
                tokio::spawn(async move {
                    if let Ok(r) = api.sections(&session_id, &gists).await {
                        let _ = tx.send(Action::Sections {
                            session_id,
                            sections: r.sections,
                        });
                    }
                });
            }
            Effect::SearchSessions(q) => {
                tokio::spawn(async move {
                    let Ok(r) = api.search(&q, Some(60)).await else {
                        return;
                    };
                    // A hit inside a COLLAPSED session (a subagent, a workflow
                    // agent) is not a row the tree can show: those surface only
                    // under their spawner on drill-in. The spawner IS the row,
                    // so the hit is attributed to it — otherwise "searches every
                    // message" quietly excludes every message a delegate wrote.
                    let mut sessions: Vec<String> = Vec::new();
                    let mut messages: Vec<String> = Vec::new();
                    for hit in r.hits {
                        let sid = match (hit.collapsed, hit.origin_id) {
                            (true, Some(origin)) => origin,
                            _ => hit.session_id,
                        };
                        if !sessions.contains(&sid) {
                            sessions.push(sid);
                        }
                        if !messages.contains(&hit.message_id) {
                            messages.push(hit.message_id);
                        }
                    }
                    let _ = tx.send(Action::SearchHits {
                        q,
                        sessions,
                        messages,
                    });
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
            // ---- row 3.20: the four remaining tabs ------------------------
            Effect::LoadWorkflows => {
                tokio::spawn(async move {
                    let sid = session.lock().expect("session lock").clone();
                    // No runs is a state, not an error: the tab says "no
                    // workflow runs in this conversation — ask for one".
                    let runs = api.list_workflows(sid.as_deref()).await.ok();
                    let _ = tx.send(Action::Workflows(
                        runs.map(|l| l.workflows).unwrap_or_default(),
                    ));
                });
            }
            Effect::LoadWorkflow(id) => {
                tokio::spawn(async move {
                    match api.get_workflow(&id).await {
                        Ok(detail) => {
                            let _ = tx.send(Action::Workflow(Some(detail)));
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                            let _ = tx.send(Action::Workflow(None));
                        }
                    }
                });
            }
            // Steer, THEN re-read: the answer to "did it pause" is the run's
            // own state, never the POST's 202.
            Effect::SteerWorkflow { id, action } => {
                tokio::spawn(async move {
                    use crate::components::panel::host::WorkflowAction as A;
                    let result = match action {
                        A::Pause => api.pause_workflow(&id).await,
                        A::Resume => api.resume_workflow(&id).await,
                        A::Stop => api.stop_workflow(&id).await,
                        A::Rerun => api.rerun_workflow(&id).await,
                    };
                    if let Err(e) = result {
                        let _ = tx.send(Action::Notice(e.to_string()));
                    }
                    if let Ok(list) = api.list_workflows(None).await {
                        let _ = tx.send(Action::Workflows(list.workflows));
                    }
                    if let Ok(detail) = api.get_workflow(&id).await {
                        let _ = tx.send(Action::Workflow(Some(detail)));
                    }
                });
            }
            Effect::SaveWorkflow(id) => {
                tokio::spawn(async move {
                    // The saved name is the run's own — the point of saving is
                    // that you watched THIS script work.
                    let name = match api.get_workflow(&id).await {
                        Ok(detail) => detail.workflow.name,
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                            return;
                        }
                    };
                    match api.save_workflow_as(&id, &name).await {
                        Ok(_) => {
                            let _ = tx.send(Action::Notice(format!(
                                "saved as {name} — run it again by name"
                            )));
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                });
            }
            Effect::LoadMcp => {
                tokio::spawn(async move {
                    let sid = session.lock().expect("session lock").clone();
                    match api.mcp_status(sid.as_deref()).await {
                        Ok(status) => {
                            let _ = tx.send(Action::Mcp(Some(status)));
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                            let _ = tx.send(Action::Mcp(None));
                        }
                    }
                });
            }
            Effect::SetMcpEnabled { name, enabled } => {
                tokio::spawn(async move {
                    let sid = session.lock().expect("session lock").clone();
                    if let Err(e) = api.set_mcp_enabled(&name, enabled, sid.as_deref()).await {
                        let _ = tx.send(Action::Notice(e.to_string()));
                    }
                    let _ = tx.send(Action::Mcp(api.mcp_status(sid.as_deref()).await.ok()));
                });
            }
            Effect::AddMcpServer { name, url } => {
                tokio::spawn(async move {
                    match api.put_mcp_server(&name, &url).await {
                        Ok(_) => {
                            // Registering GRANTS NOTHING: the row appears "off"
                            // and ⏎ is what turns it on. Authorization is named
                            // rather than started behind the user's back.
                            let _ = tx.send(Action::Notice(format!(
                                "registered {name} — ⏎ grants it to this conversation; a authorizes"
                            )));
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                    let sid = session.lock().expect("session lock").clone();
                    let _ = tx.send(Action::Mcp(api.mcp_status(sid.as_deref()).await.ok()));
                });
            }
            Effect::DeleteMcpServer(name) => {
                tokio::spawn(async move {
                    if let Err(e) = api.delete_mcp_server(&name).await {
                        let _ = tx.send(Action::Notice(e.to_string()));
                    }
                    let sid = session.lock().expect("session lock").clone();
                    let _ = tx.send(Action::Mcp(api.mcp_status(sid.as_deref()).await.ok()));
                });
            }
            // `c` REPORTS: the tool count, or the error, so "keychain" (which
            // credential will be TRIED) becomes an answer without spending a
            // turn on a tool call.
            Effect::ConnectMcpServer(name) => {
                tokio::spawn(async move {
                    match api.connect_mcp_server(&name).await {
                        Ok(v) => {
                            let tools = v
                                .get("toolCount")
                                .and_then(|n| n.as_i64())
                                .or_else(|| {
                                    v.get("tools")
                                        .and_then(|t| t.as_array())
                                        .map(|a| a.len() as i64)
                                })
                                .unwrap_or(0);
                            let _ = tx.send(Action::Notice(
                                match v.get("error").and_then(|e| e.as_str()) {
                                    Some(err) => format!("{name}: {err}"),
                                    None => format!("{name}: connected · {tools} tools"),
                                },
                            ));
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                    let sid = session.lock().expect("session lock").clone();
                    let _ = tx.send(Action::Mcp(api.mcp_status(sid.as_deref()).await.ok()));
                });
            }
            Effect::RestartMcpServer(name) => {
                tokio::spawn(async move {
                    if let Err(e) = api.restart_mcp_server(&name).await {
                        let _ = tx.send(Action::Notice(e.to_string()));
                    }
                    let sid = session.lock().expect("session lock").clone();
                    let _ = tx.send(Action::Mcp(api.mcp_status(sid.as_deref()).await.ok()));
                });
            }
            // The URL is PRINTED, never opened: the panel must not launch a
            // browser behind the keypress that asked for a token.
            Effect::BeginMcpAuth(name) => {
                tokio::spawn(async move {
                    match api.begin_mcp_auth(&name).await {
                        Ok(v) => {
                            let url = v
                                .get("url")
                                .or_else(|| v.get("authorizeUrl"))
                                .and_then(|u| u.as_str())
                                .unwrap_or("");
                            let _ = tx.send(Action::Notice(if url.is_empty() {
                                format!("{name}: no authorization URL was returned")
                            } else {
                                format!("open {url}")
                            }));
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                });
            }
            Effect::ClearMcpAuth(name) => {
                tokio::spawn(async move {
                    match api.clear_mcp_auth(&name).await {
                        Ok(_) => {
                            let _ = tx.send(Action::Notice(format!(
                                "forgot {name}'s credentials — the registration is kept"
                            )));
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                    let sid = session.lock().expect("session lock").clone();
                    let _ = tx.send(Action::Mcp(api.mcp_status(sid.as_deref()).await.ok()));
                });
            }
            // The TAB's rows, which carry `error` and `sources` — a failed
            // fetch is `None` with its reason, never an empty list.
            Effect::LoadSkillRows => {
                tokio::spawn(async move {
                    match api.list_skill_rows().await {
                        Ok(list) => {
                            let _ = tx.send(Action::SkillRows {
                                skills: Some(list.skills),
                                sources: list.sources,
                                note: None,
                            });
                        }
                        Err(e) => {
                            let _ = tx.send(Action::SkillRows {
                                skills: None,
                                sources: Vec::new(),
                                note: Some(e.to_string()),
                            });
                        }
                    }
                });
            }
            Effect::LoadModels => {
                tokio::spawn(async move {
                    // A catalog that did not answer leaves the compiled-in rows
                    // the server already merged; an empty list is not a modal.
                    if let Ok(catalog) = api.list_models().await {
                        let _ = tx.send(Action::Models(catalog.models));
                    }
                });
            }
            Effect::LoadModelSettings => {
                tokio::spawn(async move {
                    if let Ok(settings) = api.get_model_settings().await {
                        let _ = tx.send(Action::ModelSettings(settings));
                    }
                });
            }
            // BOTH halves of spec §12, in one place: the install default moves
            // and THIS session is pinned. Every other session keeps what it had
            // — nothing here can express a change to them.
            //
            // KNOWN GAP, reported rather than worked around: there is no write
            // route for the CHEAP tier (`PUT /model-settings` carries
            // `model`/`effort` only, and `cheapModel` is resolved server-side
            // from `BOUGH_CHEAP_MODEL`). A cheap pick therefore moves the ● on
            // this screen and nothing else, and the tab says so rather than
            // letting the dot claim a write that did not happen.
            Effect::SaveModel(cfg) => {
                tokio::spawn(async move {
                    use crate::components::panel::model::EffortChoice;
                    use bough_core::schema::requests::{PatchSessionBody, PutModelSettingsBody};
                    use bough_core::types::Patch;
                    let effort = match cfg.default_effort {
                        EffortChoice::Default => Patch::Clear,
                        EffortChoice::Level(e) => Patch::Set(e),
                    };
                    let body = PutModelSettingsBody {
                        model: Patch::Set(cfg.default_model.clone()),
                        effort: effort.clone(),
                    };
                    if let Err(e) = api.put_model_settings(&body).await {
                        let _ = tx.send(Action::Notice(e.to_string()));
                    }
                    let sid = session.lock().expect("session lock").clone();
                    if let Some(sid) = sid {
                        let patch = PatchSessionBody {
                            model: match &cfg.session_model {
                                Some(m) => Patch::Set(m.clone()),
                                None => Patch::Clear,
                            },
                            effort: match cfg.session_effort {
                                Some(EffortChoice::Level(e)) => Patch::Set(e),
                                Some(EffortChoice::Default) => Patch::Clear,
                                None => Patch::Keep,
                            },
                        };
                        if let Err(e) = api.patch_session(&sid, &patch).await {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                    if let Ok(settings) = api.get_model_settings().await {
                        let _ = tx.send(Action::ModelSettings(settings));
                    }
                });
            }
            // No I/O AT ALL: the reducer has already cleared the screen state,
            // and the only thing out here that remembers the old conversation
            // is the id this transport reuses on the next send. Forgetting it
            // IS starting a fresh one — the conversation itself is created by
            // whatever is sent next, exactly as it is on a cold launch.
            Effect::NewConversation => {
                *self.session.lock().expect("session lock") = None;
            }
            Effect::Compact(goal) => {
                tokio::spawn(async move {
                    let known = session.lock().expect("session lock").clone();
                    let Some(sid) = known else { return };
                    match api.handoff(&sid, &goal).await {
                        Ok(res) => {
                            // The new root is opened FIRST and the draft lands
                            // after it: `SessionOpened` clears the screen's
                            // per-conversation state, and a draft placed before
                            // it would be cleared by the switch it arrived for.
                            *session.lock().expect("session lock") = Some(res.session.id.clone());
                            let _ = tx.send(Action::SessionOpened(res.session.id.clone()));
                            let _ = tx.send(Action::Thread(Vec::new()));
                            if let Some(draft) = res.session.draft.clone() {
                                let _ = tx.send(Action::Draft(draft));
                            }
                            let _ = tx.send(Action::Notice(HANDED_OFF.to_string()));
                            // The tree's rows are now wrong — a branch was
                            // created (or a tail grew) — so they are re-read.
                            if let Ok(rows) = api.list_sessions(None).await {
                                let _ = tx.send(Action::Sessions(rows));
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                });
            }
            // A job belongs to a session, and on a fresh screen there is none —
            // so `!git status`, the first thing a user types on arriving, used
            // to hit "send a message first" and do nothing. The workspace is
            // what a shell actually needs and the TUI already knows it.
            //
            // ONE shell conversation per workspace, REUSED. Minting one per
            // command left a switcher full of one-line conversations nobody
            // opened twice. It is still a real conversation — visible, openable,
            // and where the job's output is watched — and it carries a `kind`
            // rather than a title convention, which is what lets it be found
            // again after a restart.
            Effect::RunShell(command) => {
                tokio::spawn(async move {
                    let known = session.lock().expect("session lock").clone();
                    let sid = match known {
                        Some(sid) => sid,
                        None => {
                            let existing = api.list_sessions(None).await.ok().and_then(|rows| {
                                rows.into_iter()
                                    .find(|r| {
                                        r.session.kind
                                            == bough_core::schema::parts::SessionKind::Shell
                                            && r.session.workspace == workspace
                                    })
                                    .map(|r| r.session.id)
                            });
                            let opened = match existing {
                                Some(id) => Some(id),
                                None => {
                                    let body = bough_core::schema::requests::CreateSessionBody {
                                        workspace: workspace.clone(),
                                        title: Some(SHELL_SESSION_TITLE.to_string()),
                                        kind: Some(bough_core::schema::parts::SessionKind::Shell),
                                        ..Default::default()
                                    };
                                    match api.create_session(&body).await {
                                        Ok(s) => Some(s.id),
                                        Err(e) => {
                                            let _ = tx.send(Action::Notice(e.to_string()));
                                            None
                                        }
                                    }
                                }
                            };
                            let Some(id) = opened else { return };
                            // Through the same path a tree row takes, so the
                            // thread and the rail arrive with the switch.
                            match api.get_session(&id).await {
                                Ok(snapshot) => {
                                    *session.lock().expect("session lock") = Some(id.clone());
                                    let _ = tx.send(Action::SessionOpened(id.clone()));
                                    let _ = tx.send(Action::Thread(snapshot.thread));
                                }
                                Err(e) => {
                                    let _ = tx.send(Action::Notice(e.to_string()));
                                    return;
                                }
                            }
                            id
                        }
                    };
                    if let Err(e) = api.run_shell(&sid, &command).await {
                        let _ = tx.send(Action::Notice(e.to_string()));
                        return;
                    }
                    // The rail is the shell's whole UI, so it is re-read here
                    // rather than waited for: `job.spawned` also asks for this,
                    // and one extra listing beats a row that appears a second
                    // late on the screen the user is watching.
                    if let Ok(list) = api.list_jobs(&sid).await {
                        let _ =
                            tx.send(Action::Jobs(list.jobs.into_iter().map(|r| r.job).collect()));
                    }
                });
            }
            Effect::Fork {
                session_id,
                at_message_id,
                exclusive,
                summarize_abandoned,
                editor_text,
            } => {
                tokio::spawn(async move {
                    let body = bough_core::schema::requests::ForkBody {
                        at_message_id,
                        at_part: None,
                        edited_text: None,
                        exclusive: exclusive.then_some(true),
                        summarize_abandoned: summarize_abandoned.then_some(true),
                    };
                    match api.fork(&session_id, &body).await {
                        Ok(res) => {
                            *session.lock().expect("session lock") = Some(res.session.id.clone());
                            let _ = tx.send(Action::SessionOpened(res.session.id.clone()));
                            let _ = tx.send(Action::Thread(res.thread));
                            // A user turn's own text goes back to the composer:
                            // editing it and pressing ⏎ IS the new branch.
                            if let Some(text) = editor_text {
                                let _ = tx.send(Action::Draft(text));
                            }
                            // The tree's rows are now wrong — a branch was
                            // created (or a tail grew) — so they are re-read.
                            if let Ok(rows) = api.list_sessions(None).await {
                                let _ = tx.send(Action::Sessions(rows));
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                });
            }
            Effect::Extract { session_id, picks } => {
                tokio::spawn(async move {
                    let n = picks.len();
                    match api.extract(&session_id, &picks).await {
                        Ok(res) => {
                            *session.lock().expect("session lock") = Some(res.session.id.clone());
                            let _ = tx.send(Action::SessionOpened(res.session.id.clone()));
                            let _ = tx.send(Action::Thread(res.thread));
                            // Said out loud because the source is UNTOUCHED and
                            // the screen has just changed conversations:
                            // without it, `e` looks like it MOVED the turns out.
                            let _ = tx.send(Action::Notice(format!(
                                "split into a new conversation — {} copied, the original kept its own",
                                crate::store::selectors::plural(n as i64, "turn"),
                            )));
                            // The tree's rows are now wrong — a branch was
                            // created (or a tail grew) — so they are re-read.
                            if let Ok(rows) = api.list_sessions(None).await {
                                let _ = tx.send(Action::Sessions(rows));
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                });
            }
            Effect::MoveInto {
                target_id,
                source_id,
                picks,
            } => {
                tokio::spawn(async move {
                    match api.move_into(&target_id, &source_id, &picks).await {
                        Ok(res) => {
                            *session.lock().expect("session lock") = Some(target_id.clone());
                            let _ = tx.send(Action::SessionOpened(target_id));
                            let _ = tx.send(Action::Thread(res.thread));
                            // The SERVER's count, not the caller's: duplicate
                            // picks of one message merge.
                            let _ = tx.send(Action::Notice(format!(
                                "{} copied in — the other conversation kept its own",
                                crate::store::selectors::plural(res.appended as i64, "turn"),
                            )));
                            // The tree's rows are now wrong — a branch was
                            // created (or a tail grew) — so they are re-read.
                            if let Ok(rows) = api.list_sessions(None).await {
                                let _ = tx.send(Action::Sessions(rows));
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                });
            }
            // Silent on failure, like every other rail fetch: a stale rail is a
            // stale rail, never an error card over the conversation.
            Effect::LoadSchedules => {
                tokio::spawn(async move {
                    if let Ok(rows) = api.list_schedules().await {
                        let _ = tx.send(Action::Schedules(rows));
                    }
                });
            }
            Effect::DisableSchedule(id) => {
                tokio::spawn(async move {
                    match api.set_schedule_enabled(&id, false).await {
                        Ok(row) => {
                            // The scope, in the past tense and in full: a
                            // destructive act says what it did.
                            let _ = tx.send(Action::Notice(format!(
                                "disabled schedule {} — ask the agent to re-enable it",
                                crate::store::selectors::one_line(if row.title.is_empty() {
                                    &row.prompt
                                } else {
                                    &row.title
                                }),
                            )));
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                            return;
                        }
                    }
                    if let Ok(rows) = api.list_schedules().await {
                        let _ = tx.send(Action::Schedules(rows));
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
    let api = crate::api::Api::new(crate::api::ApiOptions {
        base: None,
        fetch_fn: None,
    });
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
                        // …and so are the other two feeds it is built from.
                        | Effect::LoadWorkflows
                        | Effect::LoadSchedules
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

    // ---- row 3.20: the four remaining tabs, through the COMPOSITION ROOT ----
    //
    // The failure this pins is not a rendering bug: it is a subsystem that
    // exists, is tested, and is reachable from nothing. Every assertion below
    // drives the real keymap into the real `App` and reads the real frame, so a
    // tab wired to `PanelBody::Text("nothing to show here yet")` fails here even
    // with its own module's tests green.

    /// The chord for a tab, straight off the keymap rather than typed in twice.
    fn tab_chord(tab: crate::keys::PanelTab) -> Action {
        let chord = crate::keys::TABS
            .iter()
            .find(|t| t.id == tab)
            .expect("a tab")
            .chord;
        let c = chord.strip_prefix("ctrl+").expect("a ctrl chord");
        ctrl(c.chars().next().unwrap())
    }

    #[test]
    fn every_tab_chord_opens_its_own_surface_and_asks_for_its_own_data() {
        use crate::keys::PanelTab as T;
        for (tab, effect) in [
            (T::Workflows, Effect::LoadWorkflows),
            (T::Mcp, Effect::LoadMcp),
            (T::Skills, Effect::LoadSkillRows),
            (T::Model, Effect::LoadModels),
        ] {
            let (effects, sink) = scripted();
            let mut app = App::new(TuiOptions::default(), sink, 100, 24);
            app.apply(tab_chord(tab), 0);
            assert!(
                app.panel.open(),
                "{tab:?}: the chord did not open the panel"
            );
            assert_eq!(app.panel.tab(), tab);
            // THE WIRING GATE: the tab's fetch reached the transport. A tab
            // whose route is never called is invisible to every client.
            assert!(
                effects.borrow().contains(&effect),
                "{tab:?}: {effect:?} was never issued — {:?}",
                effects.borrow()
            );
            // …and the frame is the tab's own body, not the absent-surface
            // placeholder every unported tab used to fall through to.
            let frame = frame_of(&app, 100, 24);
            assert!(
                frame.contains(&format!("[{}]", tab.id())),
                "{tab:?}:\n{frame}"
            );
            assert!(
                !frame.contains("nothing to show here yet"),
                "{tab:?} still paints the placeholder:\n{frame}"
            );
        }
    }

    #[test]
    fn the_workflows_tab_paints_a_runs_replay_accounting_through_the_panel() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 110, 30);
        app.apply(tab_chord(crate::keys::PanelTab::Workflows), 0);
        app.apply(
            Action::Workflows(vec![crate::api::WorkflowSummary {
                id: "run-2".into(),
                name: "audit-handlers".into(),
                description: "Review every handler".into(),
                status: "running".into(),
                current_phase: Some("Verify".into()),
                agents: crate::api::WorkflowAgentCounts {
                    total: 6,
                    done: 3,
                    cached: 2,
                    running: 1,
                    queued: 0,
                    failed: 1,
                },
                created_at: 0,
                finished_at: None,
            }]),
            1,
        );
        let list = frame_of(&app, 110, 30);
        assert!(list.contains("audit-handlers"), "{list}");
        assert!(
            list.contains("2 replayed"),
            "the list hides what cost nothing:\n{list}"
        );

        // ⏎ opens it, and the detail carries the accounting spec §8 requires.
        app.apply(key(KeyCode::Enter), 2);
        app.apply(
            Action::Workflow(Some(crate::components::panel::workflows::fixtures::detail())),
            3,
        );
        let detail = frame_of(&app, 110, 30);
        assert!(detail.contains("≡ replay"), "{detail}");
        assert!(detail.contains("2 replayed"), "{detail}");
        assert!(detail.contains("2 ran live"), "{detail}");
        assert!(detail.contains("of 6"), "{detail}");
        assert!(detail.contains("≡ usage"), "{detail}");
        assert!(detail.contains("Phases"), "{detail}");
    }

    /// A live run's events must MOVE the tab. A run view that only updates on
    /// re-entry is a fan-out you have to keep closing and reopening to watch,
    /// which is the surface delegation is for reduced to a screenshot.
    #[test]
    fn a_live_runs_events_refresh_the_open_tab_and_its_narrator_line() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 110, 30);
        open_s1(&mut app);
        app.apply(tab_chord(crate::keys::PanelTab::Workflows), 0);
        app.apply(
            Action::Workflow(Some(crate::components::panel::workflows::fixtures::detail())),
            1,
        );
        app.panel.wf_level = 1;
        effects.borrow_mut().clear();
        app.apply(event(EventType::WorkflowAgent, 2, serde_json::json!({})), 2);
        let sent = effects.borrow().clone();
        assert!(sent.contains(&Effect::LoadWorkflows), "{sent:?}");
        assert!(
            sent.contains(&Effect::LoadWorkflow("run-2".into())),
            "{sent:?}"
        );

        // The narrator line lands on the header — but only for the run in view.
        app.apply(
            event(
                EventType::WorkflowLog,
                3,
                serde_json::json!({"runId": "someone-else", "line": "not this run"}),
            ),
            3,
        );
        assert_eq!(app.panel.last_log, None);
        app.apply(
            event(
                EventType::WorkflowLog,
                4,
                serde_json::json!({"runId": "run-2", "line": "dispatching Verify"}),
            ),
            4,
        );
        assert_eq!(app.panel.last_log.as_deref(), Some("dispatching Verify"));
        assert!(frame_of(&app, 110, 30).contains("▸ dispatching Verify"));

        // A CLOSED panel re-reads the LIST — the rail shows runs and is
        // visible with the panel shut — but not the detail nobody is watching.
        app.apply(ctrl('t'), 5);
        assert!(!app.panel.open());
        effects.borrow_mut().clear();
        app.apply(event(EventType::WorkflowAgent, 6, serde_json::json!({})), 6);
        assert_eq!(
            effects.borrow().clone(),
            vec![Effect::LoadWorkflows],
            "{:?}",
            effects.borrow()
        );
    }

    #[test]
    fn the_mcp_tab_paints_grant_connection_and_credential_through_the_panel() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 110, 24);
        app.apply(tab_chord(crate::keys::PanelTab::Mcp), 0);
        // The beat before the fetch lands is `loading…`, never an empty list.
        assert!(frame_of(&app, 110, 24).contains("loading…"));
        app.apply(
            Action::Mcp(Some(crate::components::panel::mcp::fixtures::status(
                &[(
                    "alpha",
                    crate::components::panel::mcp::fixtures::stdio("alpha-server"),
                )],
                &["alpha"],
                &[("alpha", false)],
                vec![],
            ))),
            1,
        );
        let frame = frame_of(&app, 110, 24);
        assert!(frame.contains("alpha"), "{frame}");
        assert!(frame.contains("granted"), "{frame}");
        assert!(frame.contains("needs auth"), "{frame}");
        assert!(
            frame.contains("F forget"),
            "the nine-key legend is cut:\n{frame}"
        );
    }

    #[test]
    fn the_skills_tab_paints_the_rows_and_never_fakes_an_empty_list() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 110, 24);
        app.apply(tab_chord(crate::keys::PanelTab::Skills), 0);
        // A failed fetch says WHY; it does not claim the user has no skills.
        app.apply(
            Action::SkillRows {
                skills: None,
                sources: Vec::new(),
                note: Some("the server did not answer /skills".into()),
            },
            1,
        );
        let failed = frame_of(&app, 110, 24);
        assert!(
            failed.contains("the server did not answer /skills"),
            "{failed}"
        );
        assert!(!failed.contains("no skills installed"), "{failed}");
        app.apply(
            Action::SkillRows {
                skills: Some(vec![crate::components::panel::skills::SkillRow {
                    name: "history".into(),
                    description: "query the db".into(),
                    error: None,
                    mcp: Vec::new(),
                }]),
                sources: vec![crate::api::SkillSourceRow {
                    source: "user".into(),
                    dir: "/home/u/.bough/skills".into(),
                }],
                note: None,
            },
            2,
        );
        let frame = frame_of(&app, 110, 24);
        assert!(frame.contains("/history"), "{frame}");
        assert!(frame.contains("query the db"), "{frame}");
        assert!(
            frame.contains("read from user /home/u/.bough/skills"),
            "{frame}"
        );
    }

    /// `/new` — a fresh conversation. Everything on this screen belonged to the
    /// one being left, the DRAFT included: carrying it over is how you send the
    /// wrong thing to the wrong thread.
    #[test]
    fn session_new_drops_the_open_conversation_and_everything_shown_for_it() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        app.apply(
            Action::Thread(vec![crate::forest::fixtures::msg("m1", Role::User, "go")]),
            1,
        );
        app.apply(Action::Jobs(vec![job("j1", "sleep 100")]), 2);
        type_text(&mut app, "half a thought", 3);
        effects.borrow_mut().clear();

        app.apply(Action::Run(Command::SessionNew, String::new()), 4);
        assert_eq!(app.session_id, None);
        assert!(app.thread.is_empty(), "the transcript is the old one's");
        assert_eq!(app.draft, "");
        assert!(app.jobs.is_empty(), "another conversation's shells");
        // The TRANSPORT is told too: it holds the id the next send would reuse,
        // and without this the fresh conversation would be the old one.
        assert_eq!(sends(&effects), vec![Effect::NewConversation]);
        let frame = frame_of(&app, 100, 24);
        assert!(frame.contains("type a message · enter sends"), "{frame}");
    }

    /// `esc esc` on an idle, empty composer: the tree, ON the turn you would go
    /// back to. A running turn still resolves to the STOP — that meaning may
    /// not be lost to the rewind.
    #[test]
    fn double_esc_on_an_empty_composer_opens_the_tree_on_your_last_turn() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 30);
        open_s1(&mut app);
        app.apply(
            Action::Thread(vec![
                crate::forest::fixtures::msg("m1", Role::User, "go"),
                crate::forest::fixtures::msg("m2", Role::Supervisor, "done"),
            ]),
            1,
        );
        app.apply(
            Action::Sessions(vec![crate::forest::fixtures::session_row(
                "s1",
                bough_core::schema::parts::SessionKind::Root,
                0,
            )]),
            1,
        );
        effects.borrow_mut().clear();
        app.apply(key(KeyCode::Esc), 10);
        assert!(!app.panel.open(), "one esc is still just a cancel");
        app.apply(key(KeyCode::Esc), 20);
        assert!(app.panel.open() && app.panel.tab() == crate::keys::PanelTab::Tree);
        assert!(app.panel.expanded.contains("s1"));
        assert_eq!(
            app.panel.rows()[app.panel.sel].id(),
            "m1",
            "the last USER turn, not the top of the forest"
        );
        assert!(sends(&effects).contains(&Effect::LoadSessions));
    }

    /// `/compact` — the handoff. It calls the model, so it announces itself
    /// before it starts; the distilled prompt lands in the COMPOSER, where it
    /// is read and edited before any of it is sent.
    #[test]
    fn compact_hands_off_and_the_distilled_prompt_lands_in_the_composer() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        // An EMPTY conversation has nothing to distil, and says so rather than
        // paying for a summary of nothing.
        effects.borrow_mut().clear();
        app.apply(Action::Run(Command::SessionCompact, String::new()), 1);
        assert_eq!(app.notice.as_deref(), Some(NOTHING_TO_HAND_OFF));
        assert!(sends(&effects).is_empty(), "{:?}", effects.borrow());

        app.apply(
            Action::Thread(vec![crate::forest::fixtures::msg("m1", Role::User, "go")]),
            2,
        );
        app.apply(
            Action::Run(Command::SessionCompact, "focus on the parser".into()),
            3,
        );
        assert_eq!(app.notice.as_deref(), Some(DISTILLING));
        assert_eq!(
            sends(&effects),
            vec![Effect::Compact("focus on the parser".into())]
        );
        // With NO goal stated the instruction has to say what "keep going"
        // means, or the summarizer guesses which thread of work it is about.
        effects.borrow_mut().clear();
        app.apply(Action::Run(Command::SessionCompact, "   ".into()), 4);
        assert_eq!(
            sends(&effects),
            vec![Effect::Compact(DEFAULT_HANDOFF_GOAL.into())]
        );

        // What the transport posts back: the new root, then its draft.
        app.apply(Action::SessionOpened("s2".into()), 5);
        app.apply(Action::Thread(Vec::new()), 5);
        app.apply(Action::Draft("carry on with the parser".into()), 5);
        app.apply(Action::Notice(HANDED_OFF.to_string()), 5);
        assert_eq!(app.draft, "carry on with the parser");
        assert_eq!(app.cursor, app.draft.chars().count());
        let frame = frame_of(&app, 100, 24);
        assert!(frame.contains("carry on with the parser"), "{frame}");
        assert!(frame.contains("handed off to a fresh"), "{frame}");
    }

    /// The rail's two verbs on the two rows that used to refuse them.
    #[test]
    fn the_rail_opens_a_run_and_stops_a_run_or_a_schedule() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        app.apply(
            Action::Workflows(vec![crate::api::WorkflowSummary {
                id: "run-1".into(),
                name: "verify".into(),
                description: String::new(),
                status: "running".into(),
                current_phase: Some("build".into()),
                agents: Default::default(),
                created_at: 0,
                finished_at: None,
            }]),
            1,
        );
        app.apply(
            Action::Schedules(vec![schedule("sch-1", "nightly", "daily@03:00", 60_000)]),
            1,
        );
        // Both rows are IN the rail — until now the app passed empty slices, so
        // neither kind could appear at all.
        let units = app.units();
        assert_eq!(units.len(), 2, "{units:?}");
        let frame = frame_of(&app, 100, 24);
        assert!(frame.contains("verify"), "{frame}");
        assert!(frame.contains("nightly"), "{frame}");

        // ⏎ on the run: the workflows tab, drilled in on it.
        app.apply(key(KeyCode::Down), 2); // into the rail
        effects.borrow_mut().clear();
        app.apply(key(KeyCode::Enter), 3);
        assert!(app.panel.open() && app.panel.tab() == crate::keys::PanelTab::Workflows);
        assert!(
            sends(&effects).contains(&Effect::LoadWorkflow("run-1".into())),
            "{:?}",
            effects.borrow()
        );

        // `x x` on the run STOPS it, and on the schedule DISABLES it.
        app.apply(ctrl('t'), 4); // close the panel, back to the rail
        app.rail_sel = Some(0);
        effects.borrow_mut().clear();
        app.apply(key(KeyCode::Char('x')), 5);
        assert!(sends(&effects).is_empty(), "one press only arms");
        app.apply(key(KeyCode::Char('x')), 6);
        assert_eq!(
            sends(&effects),
            vec![Effect::SteerWorkflow {
                id: "run-1".into(),
                action: crate::components::panel::host::WorkflowAction::Stop,
            }]
        );
        app.rail_sel = Some(1);
        effects.borrow_mut().clear();
        app.apply(key(KeyCode::Char('x')), 7);
        app.apply(key(KeyCode::Char('x')), 8);
        assert_eq!(
            sends(&effects),
            vec![Effect::DisableSchedule("sch-1".into())]
        );
    }

    /// The `/schedules` line — and the rail's ⏎ on a timer, which is the same
    /// sentence. `schedule.*` shipped with NO surface at all, so an agent could
    /// create a recurring run that spends money and nobody could see it.
    #[test]
    fn every_schedule_is_named_with_its_spec_and_when_it_next_fires() {
        assert_eq!(
            describe_schedules(&[], 0),
            "no schedules — ask the agent to add one"
        );
        let rows = vec![schedule("a", "nightly build", "daily@03:00", 3_600_000), {
            let mut off = schedule("b", "weekly sweep", "every:7d", -1);
            off.enabled = false;
            off
        }];
        assert_eq!(
            describe_schedules(&rows, 0),
            "2 schedules: daily@03:00 nightly build → next in 1h00m · \
             (off) every:7d weekly sweep → next due — ask the agent to change one"
        );
    }

    #[test]
    fn the_model_tab_paints_both_tiers_and_marks_what_is_in_force() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 110, 30);
        app.apply(tab_chord(crate::keys::PanelTab::Model), 0);
        app.apply(
            Action::Models(crate::components::panel::model::fixtures::catalog()),
            1,
        );
        app.apply(
            Action::ModelSettings(crate::api::ModelSettings {
                default_model: "claude-opus-5".into(),
                cheap_model: None,
                default_effort: None,
            }),
            2,
        );
        let frame = frame_of(&app, 110, 30);
        assert!(frame.contains("frontier model"), "{frame}");
        assert!(frame.contains("cheap model"), "{frame}");
        assert!(frame.contains("thinking depth"), "{frame}");
        assert!(frame.contains("Opus 5"), "{frame}");
        // An install with no cheap tier gets a real row, not a missing dot.
        assert!(frame.contains("(unset)"), "{frame}");
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
        let opts = TuiOptions {
            workspace: Some("/tmp/demo".into()),
        };
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
        assert!(
            sent.contains("type a message · enter sends"),
            "composer cleared: {sent}"
        );

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
            event(
                EventType::MessageDelta,
                1_100,
                json!({"messageId": "m1", "delta": "Working on"}),
            ),
            1_100,
        );
        app.apply(
            event(
                EventType::MessageDelta,
                1_200,
                json!({"messageId": "m1", "delta": " it now."}),
            ),
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
            event(
                EventType::MessageDelta,
                1_100,
                json!({"messageId": "m1", "delta": "Done."}),
            ),
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
        assert_eq!(
            hits, 1,
            "live lines are replaced by the part, not duplicated: {lines:?}"
        );
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
            event(
                EventType::MessageDelta,
                1_100,
                json!({"messageId": "m1", "delta": "half a"}),
            ),
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

    fn schedule(
        id: &str,
        title: &str,
        spec: &str,
        next_in_ms: i64,
    ) -> bough_core::schema::parts::Schedule {
        bough_core::schema::parts::Schedule {
            id: id.into(),
            title: title.into(),
            prompt: "do the thing".into(),
            workspace: None,
            session_id: None,
            spec: spec.into(),
            enabled: true,
            created_at: 0,
            last_run_at: None,
            next_run_at: next_in_ms,
        }
    }

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
        assert!(
            railed.contains("sleep 30"),
            "the rail shows the shell: {railed}"
        );

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
        assert!(sends(&effects)
            .iter()
            .all(|e| *e != Effect::KillJob("job-1".into())));
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
        assert!(
            !app.animating(),
            "an idle screen still stops the redraw loop"
        );
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
        // `!` IS THE USER'S OWN SHELL: a background job, never a turn. Nothing
        // is billed, nothing enters the thread, and the composer is cleared
        // because the line went somewhere.
        type_text(&mut app, "!echo hi", 0);
        app.apply(key(KeyCode::Enter), 10);
        assert_eq!(
            sends(&effects),
            vec![Effect::RunShell("echo hi".into())],
            "a ! line must not bill the model"
        );
        assert_eq!(app.draft, "");
        // (The composer is already empty, so this double-tap is the rewind —
        // its own test — and the panel it opens is closed again here.)
        app.apply(key(KeyCode::Esc), 20);
        app.apply(key(KeyCode::Esc), 30);
        app.apply(Action::Run(Command::PanelClose, String::new()), 31);
        effects.borrow_mut().clear();
        // An unrecognised `/word` is a command ATTEMPT, never prose: it is
        // intercepted with the teaching error and the draft is kept.
        type_text(&mut app, "/zzz", 40);
        app.apply(key(KeyCode::Enter), 50);
        assert!(sends(&effects).is_empty(), "{:?}", effects.borrow());
        assert!(
            app.notice.as_deref().unwrap().contains("there is no /zzz"),
            "{:?}",
            app.notice
        );
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
            &[Effect::Run(
                Command::Tab(crate::keys::PanelTab::Model),
                String::new()
            )]
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
        assert!(
            notice.contains("type / for the list, or ? for every key"),
            "{notice}"
        );
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
        with_files(
            &mut app,
            &["server/app.ts", "app.tsx", "components/Chat.tsx"],
        );

        let frame = frame_of(&app, 80, 24);
        assert!(frame.contains("@app.tsx"), "exact prefix leads: {frame}");
        assert!(frame.contains("files & dirs"), "{frame}");
        assert!(
            !frame.contains("Chat.tsx"),
            "a non-match is not a row: {frame}"
        );

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
            effects
                .borrow()
                .contains(&Effect::LoadDirEntries("~/".into())),
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
            effects
                .borrow()
                .contains(&Effect::LoadDirEntries("~/repos/".into())),
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
            &[Effect::Run(
                Command::Tab(crate::keys::PanelTab::Tree),
                String::new()
            )]
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
        assert_eq!(
            app.scroll_off,
            app.transcript_lines().len() - 1,
            "clamped to lines-1"
        );
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
            parts: vec![Part::Text {
                text: "see https://example.com/auth now".into(),
            }],
            pending: false,
            created_at: 1,
        });
        let painted = app.painted_rows();
        let (y, col) = painted
            .iter()
            .enumerate()
            .find_map(|(i, row)| row.find("https://").map(|b| (i, row[..b].chars().count())))
            .expect("the address is on screen");
        app.apply(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                col as u16,
                y as u16,
            ),
            0,
        );
        app.apply(
            mouse(MouseEventKind::Up(MouseButton::Left), col as u16, y as u16),
            1,
        );
        assert_eq!(
            recorded(&opened).as_slice(),
            &["https://example.com/auth".to_string()]
        );

        // One column before the address is prose, and prose opens nothing.
        app.apply(
            mouse(MouseEventKind::Down(MouseButton::Left), 0, y as u16),
            2,
        );
        app.apply(mouse(MouseEventKind::Up(MouseButton::Left), 0, y as u16), 3);
        assert_eq!(
            recorded(&opened).len(),
            1,
            "a click on prose opened nothing"
        );
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
        assert!(
            !app.busy(),
            "a foreign session's turn must not mark this one busy"
        );
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
        assert!(
            frame.contains("has the keyboard · esc returns here"),
            "{frame}"
        );
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
            event(
                EventType::MessageStarted,
                1_000,
                json!({
                    "id": "m1", "sessionId": "s1", "role": "user",
                    "parts": [{"type": "text", "text": "a transcript row"}],
                    "pending": false, "createdAt": 1_000,
                }),
            ),
            1_000,
        );
        assert!(frame_of(&app, 80, 24).contains("a transcript row"));
        app.apply(ctrl('t'), 0);
        app.apply(Action::Sessions(sessions()), 0);
        let frame = frame_of(&app, 80, 24);
        assert!(
            !frame.contains("a transcript row"),
            "the panel must displace the chat: {frame}"
        );
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
            &[Effect::Run(
                Command::Tab(crate::keys::PanelTab::Tree),
                String::new()
            )]
        );
        assert!(!app.panel.open());
        // …and the transport hands the client-owned ones straight back.
        app.apply(
            Action::Run(Command::Tab(crate::keys::PanelTab::Tree), String::new()),
            0,
        );
        assert!(app.panel.open());
        assert_eq!(app.panel.tab(), crate::keys::PanelTab::Tree);
    }

    #[test]
    fn the_help_overlay_opens_on_a_bare_question_mark_and_is_the_whole_screen() {
        let (_effects, sink) = scripted();
        let mut app = App::new(
            TuiOptions {
                workspace: Some("/w/demo".into()),
            },
            sink,
            80,
            24,
        );
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
            app.apply(
                if open == "?" {
                    key(KeyCode::Char('?'))
                } else {
                    ctrl('t')
                },
                0,
            );
            app.apply(ctrl('c'), 0);
            assert!(!app.quit, "one ^c must never tear the UI down");
            assert!(app
                .notice
                .as_deref()
                .unwrap_or("")
                .contains("^c again to quit"));
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
        assert!(!sends(&effects)
            .iter()
            .any(|e| matches!(e, Effect::Revert(_))));
        // ⏎ performs it, addressed to the path.
        app.apply(key(KeyCode::Enter), 0);
        assert!(sends(&effects).contains(&Effect::Revert(Some(vec!["src/a.ts".to_string()]))));
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
        let mut app = App::new(
            TuiOptions {
                workspace: Some("/w/demo".into()),
            },
            sink,
            80,
            24,
        );
        let down = frame_of(&app, 80, 24);
        assert!(down.contains("demo  · disconnected"), "{down}");
        app.apply(Action::Connected(true), 0);
        let up = frame_of(&app, 80, 24);
        assert!(!up.contains("disconnected"), "{up}");
    }

    // ---- the cheap-tier cosmetics (row 3.21) -------------------------------

    fn turn(id: &str, text: &str) -> Message {
        Message {
            id: id.into(),
            session_id: "s1".into(),
            role: Role::User,
            parts: vec![Part::Text { text: text.into() }],
            pending: false,
            created_at: 1,
        }
    }

    #[test]
    fn the_ghost_is_asked_for_only_on_an_idle_empty_composer_and_only_after_the_debounce() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(Action::Thread(vec![turn("m1", "hello")]), 0);
        assert!(
            !sends(&effects)
                .iter()
                .any(|e| matches!(e, Effect::GhostText(_))),
            "asked before the debounce elapsed"
        );
        app.apply(Action::Tick, GHOST_DEBOUNCE_MS + 1);
        assert_eq!(
            sends(&effects)
                .iter()
                .filter(|e| matches!(e, Effect::GhostText(_)))
                .count(),
            1,
            "exactly one ask"
        );
        // …and not again while the same conditions hold.
        app.apply(Action::Tick, GHOST_DEBOUNCE_MS + 5_000);
        assert_eq!(
            sends(&effects)
                .iter()
                .filter(|e| matches!(e, Effect::GhostText(_)))
                .count(),
            1,
        );
    }

    #[test]
    fn a_prediction_is_shown_then_taken_by_tab_and_typing_drops_it() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(Action::Thread(vec![turn("m1", "hello")]), 0);
        app.apply(Action::Ghost("run the tests".into()), 1);
        let frame = frame_of(&app, 80, 24);
        assert!(
            frame.contains("run the tests"),
            "the prediction is on screen:\n{frame}"
        );
        assert!(frame.contains("⇥ tab"), "and says how to take it:\n{frame}");
        // ⇥ REPLACES the draft with it, and the prediction is gone.
        app.apply(key(KeyCode::Tab), 2);
        assert_eq!(app.draft, "run the tests");
        assert_eq!(app.cursor, "run the tests".chars().count());
        assert_eq!(app.ghost, "");
        // A prediction that appears while you type fights you for the row.
        app.apply(Action::Ghost("something else".into()), 3);
        assert_eq!(app.ghost, "", "a non-empty composer takes no prediction");
    }

    #[test]
    fn the_ghost_never_appears_mid_turn() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        let mut pending = turn("m1", "hello");
        pending.pending = true;
        app.apply(Action::Thread(vec![pending]), 0);
        app.apply(Action::Tick, GHOST_DEBOUNCE_MS + 1);
        assert!(!sends(&effects)
            .iter()
            .any(|e| matches!(e, Effect::GhostText(_))));
    }

    #[test]
    fn topic_sections_are_asked_for_once_and_only_for_a_long_conversation() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(ctrl('t'), 0); // the tree
        app.apply(
            Action::Sessions(vec![crate::forest::fixtures::session_row(
                "s1",
                bough_core::schema::parts::SessionKind::Root,
                1,
            )]),
            0,
        );
        app.panel.expanded.insert("s1".into());
        // Short: nothing to caption.
        let short: Vec<Message> = (0..SECTION_MIN_TURNS - 1)
            .map(|i| turn(&format!("m{i}"), "short"))
            .collect();
        app.apply(Action::Thread(short), 1);
        assert!(!sends(&effects)
            .iter()
            .any(|e| matches!(e, Effect::Sections { .. })));
        let long: Vec<Message> = (0..SECTION_MIN_TURNS)
            .map(|i| turn(&format!("m{i}"), "a turn about the discount bug"))
            .collect();
        app.apply(Action::Thread(long), 2);
        let asked: Vec<&Effect> = sends(&effects)
            .iter()
            .filter(|e| matches!(e, Effect::Sections { .. }))
            .cloned()
            .collect::<Vec<_>>()
            .leak()
            .iter()
            .collect();
        assert_eq!(asked.len(), 1, "one pass per conversation");
        match asked[0] {
            Effect::Sections { session_id, gists } => {
                assert_eq!(session_id, "s1");
                assert_eq!(gists.len(), SECTION_MIN_TURNS);
                assert_eq!(gists[0], "a turn about the discount bug");
            }
            _ => unreachable!(),
        }
        // A second frame does not ask again.
        app.apply(Action::Tick, 3);
        assert_eq!(
            sends(&effects)
                .iter()
                .filter(|e| matches!(e, Effect::Sections { .. }))
                .count(),
            1
        );
        // The answer becomes caption rows over the turns beneath it.
        app.apply(
            Action::Sections {
                session_id: "s1".into(),
                sections: vec![crate::forest::SectionRange {
                    start: 0,
                    end: 3,
                    label: "the discount bug".into(),
                }],
            },
            4,
        );
        assert!(
            app.panel.rows().iter().any(
                |r| matches!(r, crate::forest::ForestRow::Section { label, .. }
                    if label == "the discount bug")
            ),
            "the header is a row"
        );
    }

    #[test]
    fn the_trees_slash_searches_every_message_after_its_own_debounce() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(ctrl('t'), 0);
        app.apply(key(KeyCode::Char('/')), 0);
        assert!(app.panel.filtering, "the buffer has the keyboard");
        // One character is not a search: FTS over every transcript is not free.
        app.apply(key(KeyCode::Char('c')), 1);
        app.apply(Action::Tick, 1_000);
        assert!(!sends(&effects)
            .iter()
            .any(|e| matches!(e, Effect::SearchSessions(_))));
        app.apply(key(KeyCode::Char('o')), 1_001);
        assert_eq!(app.panel.filter, "co");
        assert!(
            !sends(&effects)
                .iter()
                .any(|e| matches!(e, Effect::SearchSessions(_))),
            "not before the debounce"
        );
        app.apply(Action::Tick, 1_001 + SEARCH_DEBOUNCE_MS);
        assert_eq!(
            sends(&effects)
                .iter()
                .filter(|e| matches!(e, Effect::SearchSessions(q) if q == "co"))
                .count(),
            1
        );
        // The hits EXPAND their conversations — a marked turn in a collapsed
        // row is a mark nobody can see.
        app.apply(
            Action::SearchHits {
                q: "co".into(),
                sessions: vec!["other".into()],
                messages: vec!["m9".into()],
            },
            2_000,
        );
        assert!(app.panel.expanded.contains("other"));
        assert_eq!(app.panel.matched_messages, vec!["m9".to_string()]);
        // esc clears the query AND what it matched.
        app.apply(key(KeyCode::Esc), 2_001);
        assert_eq!(app.panel.filter, "");
        assert!(app.panel.matched_messages.is_empty());
    }

    #[test]
    fn a_stale_search_reply_never_marks_rows_against_a_query_typed_past() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(ctrl('t'), 0);
        app.apply(key(KeyCode::Char('/')), 0);
        type_text(&mut app, "compound", 1);
        app.apply(
            Action::SearchHits {
                q: "comp".into(),
                sessions: vec!["other".into()],
                messages: vec!["m9".into()],
            },
            2,
        );
        assert!(
            app.panel.matched_sessions.is_empty(),
            "a reply for an older query"
        );
    }

    #[test]
    fn the_tab_title_carries_the_conversation_and_whether_its_turn_runs() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        assert_eq!(app.tab_title(0), "bough", "no conversation, no claim");
        open_s1(&mut app);
        app.apply(
            event(
                EventType::MessageStarted,
                10,
                json!({
                    "id": "m1",
                    "sessionId": "s1",
                    "role": "supervisor",
                    "parts": [],
                    "pending": true,
                    "createdAt": 10,
                }),
            ),
            10,
        );
        assert_eq!(app.tab_title(0), "bough · ⠋", "a running turn spins");
        assert_eq!(app.tab_title(1), "bough · ⠙", "and the frame advances");
    }

    #[test]
    fn the_loops_timers_repeat_until_cleared_and_a_one_shot_runs_once() {
        use crate::term::TermTimers;
        let timers = LoopTimers::default();
        let fired: Rc<RefCell<Vec<&'static str>>> = Rc::default();
        let a = fired.clone();
        let repeat =
            timers.set_interval(Box::new(move || a.borrow_mut().push("keep-alive")), 5_000);
        let b = fired.clone();
        timers.set_timeout(Box::new(move || b.borrow_mut().push("clear")), 4_000);
        timers.fire(1_000);
        assert!(fired.borrow().is_empty(), "nothing is due yet");
        timers.fire(4_000);
        assert_eq!(fired.borrow().as_slice(), &["clear"], "the one-shot, once");
        timers.fire(9_000);
        assert_eq!(fired.borrow().as_slice(), &["clear", "keep-alive"]);
        timers.fire(20_000);
        assert_eq!(fired.borrow().len(), 3, "the interval keeps going");
        timers.clear_interval(repeat);
        timers.fire(40_000);
        assert_eq!(fired.borrow().len(), 3, "and stops when cleared");
    }
}
