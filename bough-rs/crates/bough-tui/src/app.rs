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
//! client" and keeps the draft rather than billing the model. The transcript
//! is `lines::build_lines` — the full port, with its tool folds and caps,
//! thinking, live `tool.log` rows, branch/job/workflow cards, the `#` margin
//! rows and the mark ledger — and `components/chat.rs` parses the SGR it bakes
//! into every row. There is ONE geometry: `lines::chat_body_height` /
//! `visible_slice` / `line_at_slot` serve both the paint and the hit-test.
//! Behavior contracts preserved: streaming render, esc interrupts a
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

use crate::clipboard::clipboard_image_path;
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
use crate::components::warn;
use crate::format::{
    active_trigger, apply_completion, browse_prefix, rank_completions, Candidate, Ranked, Trigger,
    TriggerKind, COMPLETION_LIMIT,
};
use crate::keys::{
    lookup, slash_invocation, strip_ctl, tab_for_command, unknown_command, Command, KeyContext,
    KeyFlags, UiMode, SLASH_COMMANDS,
};
use crate::paste::{expand_pastes, paste_mark, QUEUE_ABOVE_CHARS};
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
    /// The same, for the ONE shell conversation per workspace that `!cmd`
    /// borrows when there is nothing open yet. It is a real conversation — its
    /// jobs are on the rail and its output is readable — but it is NOT the one
    /// you are chatting in, and this is what says so: the next ordinary message
    /// starts a fresh conversation rather than being typed into `shell`.
    ShellSessionOpened(String),
    /// A pasteboard/paste read that turned out to be words. The reducer decides
    /// whether they are inlined or held (`paste.rs`).
    PasteText(String),
    /// An uploaded image, ready to ride the next message.
    Attached(bough_core::schema::requests::PostMessageImage),
    /// A transport failure worth a row (an `ApiFailure`'s own sentence).
    Notice(String),
    /// The POST for this optimistic echo never landed. The bubble comes back
    /// OUT of the transcript and the words go back into the composer — a `you`
    /// row for a message the server never saw is the one thing the screen must
    /// never show, and it used to sit there forever.
    SendFailed {
        local_id: String,
        text: String,
    },
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
    /// `GET /sessions?originId=` — the drill-in for ONE origin. The plain
    /// listing excludes the collapsing kinds, so this is the only wire a
    /// subagent, a workflow agent or a schedule run ever arrives on.
    ChildSessions {
        origin_id: String,
        rows: Vec<crate::api::SessionRow>,
    },
    /// `GET /sessions/:id` reduced to its thread, for a tree row that is NOT
    /// the open conversation.
    ForeignThread {
        session_id: String,
        thread: Vec<Message>,
    },
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
    Jobs(Vec<crate::api::JobListRow>),
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
    /// `GET /saved-workflows` — the scripts saved by name, for `/saved`.
    Saved(Vec<bough_core::workflow::saved::SavedWorkflow>),
    /// `GET /sessions/:id/artifacts` — what this conversation published, for
    /// `/artifacts`.
    Artifacts(Vec<bough_core::hostfn::artifact::Artifact>),
    /// The `AGENTS.md` files the next turn will inject, RE-READ for `/rules`
    /// rather than taken off the snapshot this screen was opened with.
    ProjectRules(Vec<crate::api::ProjectRuleSummary>),
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
    /// Everything on `GET /sessions/:id` that is NOT the thread: the session
    /// row the meter reads, the model the next turn will call, its context
    /// window, the primed tags and the injected `AGENTS.md` files. Carried
    /// apart from [`Action::Thread`] because it refreshes on a different beat
    /// (store.ts folds both out of one snapshot; here the thread arrives from
    /// the event stream and this is polled).
    SessionMeta(Box<SessionMeta>),
    /// `GET /sessions/:id/usage` — spend, without the thread. Polled, so the
    /// meter's cost is live DURING a turn rather than only after it.
    Usage(crate::api::SnapshotUsage),
    /// `GET /fs/branch?dir=` — the branch the meter's `dir@branch` names. The
    /// directory travels back with it: a reply for the checkout the screen has
    /// already left must not label the one it moved to.
    Branch {
        dir: String,
        branch: String,
    },
}

/// The meter's half of a session snapshot (store.ts::"snapshot").
#[derive(Clone, Debug)]
pub struct SessionMeta {
    pub session: bough_core::schema::parts::Session,
    pub usage: crate::api::SnapshotUsage,
    pub effective_model: Option<String>,
    pub context_limit: Option<i64>,
    pub primed_tags: Vec<String>,
    pub project_rules: Vec<crate::api::ProjectRuleSummary>,
}

/// Outbound calls. The loop never does I/O itself; the transport does.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// POST the draft as a user message, with whatever ⌘v attached to it.
    Send {
        text: String,
        images: Vec<bough_core::schema::requests::PostMessageImage>,
        /// The optimistic echo this send is for. If the POST never lands, that
        /// bubble is a LIE — an ordinary `you` row, between two real turns, for
        /// a message the server never received — so the failure path names it
        /// and the reducer takes it back out.
        local_id: String,
    },
    /// ⌃v: read the pasteboard, and attach or insert whatever it holds.
    /// The read and the upload are I/O; the DECISION is the reducer's
    /// ([`App::on_paste_text`]), which is why this hands its result back as an
    /// action rather than editing the draft from a task.
    ImagePaste,
    /// A bracketed paste whose text names an image FILE (`clipboard.rs`): read
    /// those bytes and upload them. A read that fails is a text paste, which is
    /// the honest fallback — the string may be a path the user meant to type.
    AttachPath(String),
    /// The take-back for a message still carrying its optimistic `local-N` id.
    ///
    /// There is no server id to unsend yet, so the id is RESOLVED first: the
    /// session is re-read, its last user message is compared against the text
    /// that was sent, and only an exact match is retracted. Without the compare
    /// a POST still in flight would retract the message BEFORE it — the wrong
    /// one, silently.
    UnsendLatest {
        text: String,
    },
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
    /// GET `/sessions?originId=` — the drill-in for one conversation. Fired for
    /// the OPEN conversation on the rail's beat (a subagent is live work and
    /// the rail is where live work goes) and lazily for any row expanded in the
    /// tree.
    LoadChildSessions(String),
    /// GET `/sessions/:id`, kept as a foreign thread — a tree row's turns.
    LoadForeignThread(String),
    /// Re-read the OPEN conversation's thread from the server and replace what
    /// is on screen with it. The reconnect's reconciliation.
    ReloadThread,
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
    /// `GET /saved-workflows` — `/saved`.
    LoadSaved,
    /// `GET /sessions/:id/artifacts` — `/artifacts`.
    LoadArtifacts,
    /// `GET /sessions/:id` for its `projectRules` alone — `/rules`. Re-read on
    /// purpose: the files are read from disk per turn, so an answer off the
    /// snapshot this screen opened with is exactly the stale reassurance the
    /// command exists to avoid.
    LoadProjectRules,
    /// `GET /sessions/:id` for everything on it EXCEPT the thread — the model,
    /// the context window, the tags, the rules. The thread is the event
    /// stream's business, and re-seating it from a poll would fight the text
    /// still streaming into it (`Action::SessionMeta`).
    LoadSessionMeta,
    /// `GET /sessions/:id/usage` — the spend meter, live between rounds.
    PollUsage,
    /// `GET /fs/branch?dir=` for the workspace the meter names.
    LoadBranch(String),
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

/// How often the meter re-reads the workspace's branch, in spinner ticks
/// (~10s, App.tsx::BRANCH_POLL_MS). Slow on purpose: a branch changes when a
/// human checks one out in another terminal, which is minutes apart, not
/// seconds, and this is one `git rev-parse`.
const BRANCH_POLL_TICKS: u64 = 83;

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

/// The armed-quit row. Named because two places must agree on it: the one that
/// raises it and the one that retracts it when typing disarms the confirm.
pub const QUIT_CONFIRM: &str = "^c again to quit — subagents and workflows keep running";

/// The optimistic echo's id prefix. A message wearing one has been POSTED but
/// not yet read back, so the server has never heard this name and no route may
/// be handed it (see `App::take_back`).
pub const LOCAL_ID_PREFIX: &str = "local-";

/// Said when the take-back arrives before the message it is taking back does.
/// Honest about the race and about what to do: the window is three seconds and
/// the round trip is milliseconds, so the second press lands.
pub const TAKE_BACK_TOO_SOON: &str =
    "that message has not reached the conversation yet — press esc again";

/// ⌃v with nothing on the pasteboard this TUI can use.
pub const CLIPBOARD_EMPTY: &str = "clipboard has no text or supported image";

/// The upload's receipt as the message body wants it: same four fields, one
/// crate over. `POST /attachments` has already copied the bytes somewhere
/// durable, so what rides the message is a path and never the picture again.
fn as_image(a: crate::api::Attachment) -> bough_core::schema::requests::PostMessageImage {
    bough_core::schema::requests::PostMessageImage {
        path: a.path,
        media_type: a.media_type,
        name: a.name,
        size: a.size,
    }
}

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
            | Command::SavedShow
            | Command::ArtifactsShow
            | Command::RulesShow
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

/// `/saved` — the scripts saved by name (store.ts::describeSavedWorkflows).
///
/// The empty sentence points at the ONE gesture that creates one. The non-empty
/// one deliberately does not say "ask the agent to run one by name": no host
/// function does that, and naming an action that cannot be taken is the same
/// defect as a panel that does not exist. `r` in the workflows tab is the verb.
pub fn describe_saved_workflows(rows: &[bough_core::workflow::saved::SavedWorkflow]) -> String {
    use crate::store::selectors::plural;
    if rows.is_empty() {
        return "no saved workflows — open a run in ^w and press s to save its script".to_string();
    }
    format!(
        "{}: {} — open a run in ^w and press r to re-run its script",
        plural(rows.len() as i64, "saved workflow"),
        rows.iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>()
            .join(" · "),
    )
}

/// `/artifacts` — NAMES, NOT URLS (store.ts::describeArtifacts).
///
/// A notice is ONE line, and one artifact's name plus its
/// `http://127.0.0.1:4325/artifacts/<uuid>/<file>` href is 111 characters — so
/// a list of hrefs was clipped mid-URL on a 100-column screen and the clipped
/// half was the only part the reader wanted. The full link is already in the
/// transcript on the turn that published it, wrapped and clickable; this says
/// WHAT exists and where the links are.
pub fn describe_artifacts(rows: &[bough_core::hostfn::artifact::Artifact]) -> String {
    use crate::store::selectors::plural;
    if rows.is_empty() {
        return "this conversation has published no artifacts".to_string();
    }
    format!(
        "{}: {} — the link is on the turn that published each one",
        plural(rows.len() as i64, "artifact"),
        rows.iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// `/rules` — which `AGENTS.md` files the next turn will inject, in prompt
/// order (store.ts::describeProjectRules).
pub fn describe_project_rules(rows: &[crate::api::ProjectRuleSummary]) -> String {
    use crate::store::selectors::plural;
    if rows.is_empty() {
        return "no AGENTS.md applies here — write one in the workspace, or in $BOUGH_HOME for every project"
            .to_string();
    }
    format!(
        "{} in every turn's prompt, in this order: {}{}",
        plural(rows.len() as i64, "AGENTS.md"),
        rows.iter()
            .map(|r| format!("{} ({} chars)", r.path, r.bytes))
            .collect::<Vec<_>>()
            .join(" → "),
        if rows.len() > 1 {
            " — the last one wins where two disagree"
        } else {
            ""
        },
    )
}

/// The two sentences the id-copy answers with, and the one it refuses with.
/// `^g`: the id is the handle every out-of-band route to this conversation
/// needs — a `session_id =` filter over the history tables, a `bough`
/// subcommand, a bug report naming the run — and the TUI never showed it, so it
/// was read off the database by hand. The copy path is the same OSC 52 a drag
/// uses, so it survives ssh and tmux; the id is in the notice either way, which
/// is readable and selectable rather than a gesture that silently did nothing.
pub const NO_CONVERSATION_TO_COPY: &str = "no conversation is open yet";
/// `/artifacts` and `/rules` before the first turn: neither has an answer that
/// is about anything, and both say WHY rather than showing an empty list.
pub const NO_CONVERSATION_ARTIFACTS: &str = "no conversation is open, so it has no artifacts";
pub const NO_CONVERSATION_RULES: &str = "no conversation is open, so nothing is injected yet";

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
    // ---- reading: the folds (App.tsx `foldAll`/`openKeys`/`fullKeys`) ------
    /// `^e`: every tool call and every thinking block, open at once.
    fold_all: bool,
    /// Groups the reader opened ONE AT A TIME, and blocks whose line cap they
    /// lifted. `build_lines` has always taken `is_expanded(key)`/`is_full(key)`
    /// per group and every `click` target `lines.rs` emits resolves to one of
    /// these; without them the only fold control in the product was
    /// all-or-nothing and a row that said "click to expand" was an instruction
    /// to do something impossible. `^e` still flips everything at once; it also
    /// clears these, so the global toggle stays the thing that wins.
    open_keys: HashSet<String>,
    full_keys: HashSet<String>,
    notice: Option<String>,
    /// The text the expiry timer is armed for, and the moment it was armed.
    /// Changing the sentence re-arms; leaving it alone lets it run out.
    notice_armed: Option<String>,
    notice_at: Option<i64>,
    quit_armed: bool,
    pub quit: bool,
    thread: Vec<Message>,
    /// message id → streamed-but-unfinalized text.
    streaming: HashMap<String, String>,
    /// call id → the lines a RUNNING program has printed so far (`tool.log`).
    /// `build_lines` renders these under a call with no result yet, and the
    /// finalized output REPLACES them when the `tool_result` lands.
    tool_logs: HashMap<String, Vec<String>>,
    /// The permanent ledger this conversation's transcript interleaves: how
    /// each turn settled. Memory-only — the server does not store it, so a
    /// session switch is the only thing that clears it.
    marks: Vec<crate::store::state::TranscriptMark>,
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
    ///
    /// RESET WHENEVER THE CANDIDATE SET CHANGES, not only on accept. Clamping
    /// alone (`sel_at`) is not enough: highlight row 4, type one more character
    /// until two rows are left, and ⏎ ran the row the clamp landed on — a
    /// `/command` the user never selected and never saw highlighted.
    completion_sel: usize,
    // ---- paste, attachments and the ↑ history ------------------------------
    /// Pastes held aside instead of inlined, addressed by the `[Pasted text #N]`
    /// marks they left in the draft. The ordinal is the index here plus one and
    /// never changes (`paste.rs`).
    pastes: Vec<String>,
    /// Images queued under the composer for the next message.
    attachments: Vec<bough_core::schema::requests::PostMessageImage>,
    /// Every line SENT from this screen, sigil and all, for ↑/↓ recall. One
    /// ring, so `!git status` is re-run with ↑⏎ like anything else.
    sent_history: Vec<String>,
    /// Where ↑ has walked to in `sent_history`. `None` = not recalling, and ↓
    /// off the end returns there with an empty draft.
    hist_at: Option<usize>,
    /// The open conversation is the workspace's `shell` session, borrowed by a
    /// `!` line. See [`Action::ShellSessionOpened`].
    shell_session: bool,
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
    jobs: Vec<crate::api::JobListRow>,
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
    // ---- what the status bar and the margin rows are made of ---------------
    /// The open conversation's row: its model pin, its effort, its context
    /// tokens, and whether another conversation spawned it. None until the
    /// first snapshot lands, and every field of the meter it feeds degrades to
    /// silence rather than to a zero.
    session: Option<bough_core::schema::parts::Session>,
    /// Session totals, refreshed by the usage poll AND by every snapshot — a
    /// path that updated one and not the other reported a turn as free.
    usage: Option<crate::api::SnapshotUsage>,
    /// The model the NEXT turn will actually call, when the session pins none.
    effective_model: Option<String>,
    /// What a NEW conversation would run on: the meter's model before a
    /// session exists, which is the one screen where you are about to commit
    /// to spending and the only one that would otherwise not say on what.
    default_model: Option<String>,
    /// The effective model's context window. None = unknown, and the chip then
    /// says tokens rather than an invented percentage.
    context_limit: Option<i64>,
    /// The `#` margin rows' contents — the transcript's first rows.
    primed_tags: Vec<String>,
    project_rules: Vec<String>,
    /// The branch the workspace is checked out on, for `dir@branch`, and the
    /// directory it was read FOR. A branch kept across a switch to another
    /// checkout is a claim about a directory that is no longer on screen.
    branch: Option<String>,
    branch_dir: Option<String>,
    /// One `GET /model-settings` per process, like the TS's.
    model_settings_requested: bool,
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
            fold_all: false,
            open_keys: HashSet::new(),
            full_keys: HashSet::new(),
            notice: None,
            notice_armed: None,
            notice_at: None,
            quit_armed: false,
            quit: false,
            thread: Vec::new(),
            streaming: HashMap::new(),
            tool_logs: HashMap::new(),
            marks: Vec::new(),
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
            pastes: Vec::new(),
            attachments: Vec::new(),
            sent_history: Vec::new(),
            hist_at: None,
            shell_session: false,
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
            session: None,
            usage: None,
            effective_model: None,
            default_model: None,
            context_limit: None,
            primed_tags: Vec::new(),
            project_rules: Vec::new(),
            branch: None,
            branch_dir: None,
            model_settings_requested: false,
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
        self.busy()
            || !self.jobs.is_empty()
            || self.job.is_some()
            || self.just_sent()
            // A NOTICE PENDING EXPIRY IS SOMETHING THAT MOVES. The tick is what
            // retires it, and on an idle screen there is nothing else to make
            // one happen — so without this the ten-second life is only ever
            // served to a user who was already typing, and everyone else keeps
            // the row forever, which is the bug it was meant to fix.
            || self.notice_at.is_some()
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

    // ---- the status bar's facts --------------------------------------------

    /// The checkout the meter names and the branch is read for: the open
    /// session's own, else the one this client was launched on (App.tsx's
    /// `state.session?.workspace ?? defaultWorkspace`).
    fn meter_workspace(&self) -> Option<String> {
        self.session
            .as_ref()
            .and_then(|s| s.workspace.clone())
            .or_else(|| self.options.workspace.clone())
    }

    /// The status line's whole content (App.tsx's `<StatusLine meter=…>`).
    /// Every field is what is KNOWN — nothing here invents a number, and the
    /// renderer turns each absence into silence.
    fn meter(&self) -> ChatMeter {
        let session = self.session.as_ref();
        let units = self.units();
        let count = |kind: crate::store::selectors::LiveUnitKind| -> Option<i64> {
            Some(units.iter().filter(|u| u.kind == kind).count() as i64)
        };
        ChatMeter {
            // The session's own pin first, then what the next turn would
            // actually call, then what a NEW conversation would run on.
            model: session
                .and_then(|s| s.model.clone())
                .or_else(|| self.effective_model.clone())
                .or_else(|| self.default_model.clone()),
            effort: session.and_then(|s| s.effort.clone()),
            // The tree total when there is one: a delegated turn's spend is
            // this conversation's spend.
            cost_usd: self
                .usage
                .as_ref()
                .map(|u| u.tree.cost_usd)
                .filter(|c| *c > 0.0)
                .or_else(|| self.usage.as_ref().map(|u| u.totals.cost_usd)),
            context_tokens: session.and_then(|s| s.context_tokens),
            context_limit: self.context_limit,
            workspace: self.meter_workspace(),
            branch: self.branch.clone(),
            shells: count(crate::store::selectors::LiveUnitKind::Shell),
            // The rail answers these in detail and the PANEL displaces the
            // rail, so without them a tree-tab visit makes running subagents
            // invisible everywhere at once.
            agents: count(crate::store::selectors::LiveUnitKind::Subagent),
            runs: count(crate::store::selectors::LiveUnitKind::Workflow),
            help: true,
            // Only when there IS somewhere to go back to — the same condition
            // the key is guarded on, so chip and binding cannot disagree.
            out: session.is_some_and(|s| s.origin_id.is_some()),
        }
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
        // BOTH lists, deduped by id. The plain listing carries forks and
        // handoffs; the drill-in carries the collapsing kinds — subagents among
        // them — which `GET /sessions` excludes by design (server sessions.rs's
        // "derived visibility"). Reading only the first is what made a running
        // subagent invisible on the rail, in the transcript and in the tree at
        // once.
        let mut children: Vec<crate::api::SessionRow> = self
            .panel
            .sessions
            .iter()
            .filter(|s| {
                s.session.parent_id.as_deref() == Some(current)
                    || s.session.origin_id.as_deref() == Some(current)
            })
            .cloned()
            .collect();
        if let Some(drilled) = self.panel.children_by_origin.get(current) {
            for row in drilled {
                if !children.iter().any(|c| c.session.id == row.session.id) {
                    children.push(row.clone());
                }
            }
        }
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
        // `live_units` was ported against the bare `BackgroundJob`; the rows
        // this client holds carry the tail the transcript's job card needs, so
        // the adaptation happens here rather than in the selector.
        let shells: Vec<BackgroundJob> = self.jobs.iter().map(|r| r.job.clone()).collect();
        live_units(&shells, &subagents, &runs, &self.schedules, self.now_ms)
    }

    /// A NOTICE IS A FLASH, NOT A FIXTURE. The ported store already decided how
    /// long one lives (`store::shell::NOTICE_TTL_MS`, armed from the state
    /// transition) and this client never used it: `self.notice` was set-only,
    /// cleared by `/new`, `^t` and the take-back tick alone. Everything else
    /// stayed on screen forever and rode a session switch into a conversation it
    /// said nothing about.
    ///
    /// Arming happens HERE rather than at the ~18 assignment sites: the compare
    /// against the last armed text runs after every action, so a write from any
    /// arm — including one that took an early `return` — is stamped without the
    /// site knowing about it.
    fn arm_notice(&mut self, now_ms: i64) {
        if self.notice != self.notice_armed {
            self.notice_armed = self.notice.clone();
            self.notice_at = self.notice.as_ref().map(|_| now_ms);
        }
    }

    fn expire_notice(&mut self, now_ms: i64) {
        if let Some(at) = self.notice_at {
            if now_ms.saturating_sub(at) >= crate::store::shell::NOTICE_TTL_MS as i64 {
                self.clear_notice();
            }
        }
    }

    /// Drop the row AND its timer together — a stale `notice_at` under a fresh
    /// notice would expire it early.
    fn clear_notice(&mut self) {
        self.notice = None;
        self.notice_armed = None;
        self.notice_at = None;
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
        // The notice's clock brackets the action rather than sitting inside it:
        // expire what is stale BEFORE the action reads it, arm whatever the
        // action wrote AFTER it. Any arm may `return` early, so neither half can
        // live in the body — and a path added later cannot forget to stamp.
        self.expire_notice(now_ms);
        self.apply_inner(action, now_ms);
        self.arm_notice(now_ms);
    }

    fn apply_inner(&mut self, action: Action, now_ms: i64) {
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
                // The install-wide facts, once per process: what a NEW
                // conversation would run on, so the meter names a model on the
                // one screen that has no session to name one (App.tsx).
                if !self.model_settings_requested {
                    self.model_settings_requested = true;
                    self.transport.effect(Effect::LoadModelSettings);
                }
                // The branch, on its own slow beat: a checkout happens in
                // ANOTHER terminal, so there is no event to hang it on, and one
                // `git rev-parse` every ten seconds is cheaper than a status
                // bar naming the branch you left. Fired on the first tick too —
                // otherwise the bar reads `dir` alone for ten seconds.
                if self.poll_tick == 1 || self.poll_tick.is_multiple_of(BRANCH_POLL_TICKS) {
                    if let Some(dir) = self.meter_workspace() {
                        self.transport.effect(Effect::LoadBranch(dir));
                    }
                }
                if self.poll_tick.is_multiple_of(POLL_TICKS) && self.session_id.is_some() {
                    self.transport.effect(Effect::PollJobs);
                    // Spend, without the thread — what makes the cost chip move
                    // DURING a turn rather than only after it (store.ts's
                    // `refreshUsage`, on the one timer this loop already has).
                    self.transport.effect(Effect::PollUsage);
                    // The context chip's numerator is written per round, so
                    // while a turn runs the snapshot is the only thing that
                    // moves it. Idle, nothing changes it and nothing is asked.
                    if self.busy() {
                        self.transport.effect(Effect::LoadSessionMeta);
                    }
                    // The rail's agent rows come from the listing, and a fan-out
                    // that started since the last read is exactly what it is for.
                    self.transport.effect(Effect::LoadSessions);
                    // …and the drill-in beside it, because that listing is the
                    // ONLY wire a subagent arrives on. One extra request per
                    // beat, for the open conversation alone — not per row.
                    if let Some(sid) = self.session_id.clone() {
                        self.transport.effect(Effect::LoadChildSessions(sid));
                    }
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
            Action::SendFailed { local_id, text } => {
                let before = self.thread.len();
                self.thread.retain(|m| m.id != local_id);
                if self.thread.len() == before {
                    // Already reconciled — the id was renamed by a snapshot, so
                    // the message DID land and there is nothing to take back.
                    return;
                }
                self.mirror_thread();
                // The take-back window is over the moment there is nothing to
                // take back, and the words are handed back rather than lost:
                // the user typed them, and the server never got them.
                self.last_send_at = None;
                if self.draft.is_empty() {
                    self.cursor = text.chars().count();
                    self.draft = text;
                }
                self.scroll_off = 0;
            }
            Action::Schedules(rows) => {
                self.schedules = rows;
                if std::mem::take(&mut self.describe_schedules) {
                    self.notice = Some(describe_schedules(&self.schedules, self.now_ms));
                }
            }
            // The three one-shot listings. None of them is state: they are read
            // when asked for and said once, so nothing is cached to go stale.
            Action::Saved(rows) => self.notice = Some(describe_saved_workflows(&rows)),
            Action::Artifacts(rows) => self.notice = Some(describe_artifacts(&rows)),
            Action::ProjectRules(rows) => self.notice = Some(describe_project_rules(&rows)),
            Action::Connected(up) => {
                // RECONNECTING IS A RECONCILIATION. Nothing else closes the
                // gap: the events that would have finished a turn were emitted
                // while this client was not listening, so a turn that died with
                // the server left `⠋ working · Nm` counting up forever — and
                // neither esc nor the stream coming back cleared it.
                //
                // The server marks orphaned turns at boot (bough-server
                // boot.rs's recovery), so its snapshot is the truth about what
                // is still running. Re-reading it is the whole fix.
                let was_down = !self.connected;
                self.connected = up;
                if up && was_down && self.session_id.is_some() {
                    self.transport.effect(Effect::ReloadThread);
                    self.transport.effect(Effect::LoadSessionMeta);
                    self.transport.effect(Effect::PollJobs);
                }
            }
            Action::ShellSessionOpened(id) => {
                self.apply(Action::SessionOpened(id), now_ms);
                self.shell_session = true;
                return;
            }
            Action::SessionOpened(id) => {
                self.shell_session = false;
                // …and so does what the composer was holding for the one being
                // left (see `start_fresh_conversation`). Harmless on the send
                // path that creates a conversation: `submit` has already taken
                // the queue with the message.
                self.attachments.clear();
                self.pastes.clear();
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
                // …and everything the meter and the margin rows said about the
                // conversation being left. A model, a spend and a rule sheet
                // belonging to another session are worse than none: they are
                // wrong about the one on screen. Re-read, never carried.
                self.session = None;
                self.usage = None;
                self.effective_model = None;
                self.context_limit = None;
                self.primed_tags.clear();
                self.project_rules.clear();
                self.transport.effect(Effect::PollJobs);
                self.transport.effect(Effect::LoadQuestions);
                self.transport.effect(Effect::LoadSessionMeta);
                // …and the rest of the rail's feed. Both are then kept fresh by
                // events rather than by a poll (`reduce_event`), which is the
                // TS's policy and the reason neither has a timer.
                self.transport.effect(Effect::LoadWorkflows);
                self.transport.effect(Effect::LoadSchedules);
            }
            Action::Thread(thread) => {
                self.thread = thread;
                self.streaming.clear();
                self.tool_logs.clear();
                // A switch lands at the live tail, like every arrival.
                self.scroll_off = 0;
            }
            Action::SessionMeta(meta) => {
                // A snapshot that lost the race with a session switch says
                // nothing about the session on screen (store.ts drops it).
                if self.session_id.as_deref() != Some(meta.session.id.as_str()) {
                    return;
                }
                let meta = *meta;
                self.usage = Some(meta.usage);
                // `?? state.x` and not a blind overwrite: a server older than
                // one of these fields answers null for it, and null must leave
                // what is known standing rather than blank the meter.
                if meta.effective_model.is_some() {
                    self.effective_model = meta.effective_model;
                }
                if meta.context_limit.is_some() {
                    self.context_limit = meta.context_limit;
                }
                self.primed_tags = meta.primed_tags;
                self.project_rules = meta.project_rules.into_iter().map(|r| r.label).collect();
                // The workspace may have moved with the session: the branch on
                // screen must be the branch of the checkout on screen.
                let moved = meta.session.workspace.is_some()
                    && meta.session.workspace != self.branch_dir
                    && meta.session.workspace.as_deref() != self.options.workspace.as_deref();
                self.session = Some(meta.session);
                if moved {
                    if let Some(dir) = self.meter_workspace() {
                        self.transport.effect(Effect::LoadBranch(dir));
                    }
                }
            }
            Action::Usage(usage) => self.usage = Some(usage),
            Action::Branch { dir, branch } => {
                if self.meter_workspace().as_deref() != Some(dir.as_str()) {
                    return; // a reply for a checkout this screen has left
                }
                // Empty is not a repo: silence, never an `@` with nothing after it.
                self.branch = (!branch.is_empty()).then_some(branch);
                self.branch_dir = Some(dir);
            }
            Action::Sessions(sessions) => self.panel.set_sessions(sessions),
            Action::ChildSessions { origin_id, rows } => {
                self.panel.set_children(origin_id, rows);
                // The rail is built from these, and a fan-out that just ended
                // must not leave the cursor past the last row.
                self.clamp_rail();
            }
            Action::ForeignThread { session_id, thread } => {
                // NEVER over the open conversation: `mirror_thread` owns that
                // key, and a snapshot landing late would roll back a streaming
                // turn to whatever the server had when the fetch left.
                if self.session_id.as_deref() != Some(session_id.as_str()) {
                    self.panel.threads.insert(session_id, thread);
                }
            }
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
            Action::Theme(state) => {
                // The BOOT fetch paints: the stored theme must be in force
                // before the picker is ever opened, or the one theme never
                // painted is the one the user chose. A later answer only seeds
                // the picker — applying it would stamp on a live preview.
                if self.panel.theme.is_none() {
                    crate::theme::apply_theme(state.as_ref());
                }
                self.panel.set_theme(state);
            }
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
                // The meter's last fallback: before a session exists there is
                // no pin and no effective model, and this is the screen where
                // you are about to commit to spending.
                self.default_model = Some(settings.default_model.clone());
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
            // A LIST THAT ARRIVES IS A LIST THAT CHANGED. The popup's cursor
            // must not stay on the row number it held against the placeholder
            // set, or the first ⏎ after a fetch acts on a row nobody looked at.
            Action::Files(files) => {
                self.files = files;
                self.completion_sel = 0;
            }
            Action::DirEntries { prefix, entries } => {
                self.browsed = (prefix, entries);
                self.completion_sel = 0;
            }
            Action::Skills(skills) => {
                self.skills = skills;
                self.completion_sel = 0;
            }
            Action::PasteText(text) => self.on_paste_text(&text),
            Action::Attached(image) => self.attachments.push(image),
            Action::Notice(text) => self.notice = Some(text),
            Action::Event(event) => self.reduce_event(event),
            Action::Term(TermEvent::Resize(w, h)) => {
                self.cols = w.max(20);
                self.rows = h.max(8);
            }
            Action::Term(TermEvent::Mouse(m)) => self.on_mouse(m),
            Action::Term(TermEvent::Key(k)) => self.on_key(k, now_ms),
            // A REAL PASTE IS ONE EVENT, NOT N KEYSTROKES. `enter_tui` turns
            // bracketed paste on (`?2004h`) and crossterm therefore delivers a
            // paste whole — and nothing claimed it, so every paste into this
            // TUI was silently swallowed. It is claimed here rather than in
            // `on_key` because it is not a key: no chord resolves, no
            // completion arms, and the whole burst lands at the cursor at once.
            Action::Term(TermEvent::Paste(text)) => self.on_paste(&text),
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
        // COMPARE THE CONTENT, NOT THE COUNT. A streaming assistant message is
        // already in the thread with empty parts, so the text arriving into it
        // never changes the length — and the tree went on printing
        // `bough (no text)` over a turn that had plenty.
        if self.panel.threads.get(&id) != Some(&self.thread) {
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
            return;
        }
        // A markdown link in the transcript. Its address lives in an OSC 8
        // marker, which a ratatui cell CANNOT carry — the painted row above has
        // only the label — so it is read off the styled row instead.
        if let Some(line) = self.transcript_line_at(at) {
            if let Some(url) = link_at(&line.text, here) {
                self.open_link(&url);
                return;
            }
        }
        // No link under the pointer: the row's own target, if it has one. This
        // is what makes a transcript clickable — `lines.rs` has emitted a
        // target on every foldable group, every capped block and every branch
        // card since it was written, and nothing read them.
        if let Some(target) = self.transcript_click_target(at) {
            self.click_target(&target);
        }
    }

    /// 1-based screen row the transcript starts on: the header owns row 1.
    const CHAT_TOP: u16 = 2;

    /// The completion popup's height right now — the renderer's own derivation,
    /// because the transcript's height is measured against it.
    fn popup_height(&self) -> u16 {
        match self.trigger() {
            Some(_) => {
                let ranked = self.completion();
                let more = ranked.total.saturating_sub(ranked.items.len());
                completion_popup_height(ranked.items.len(), more) as u16
            }
            None => 0,
        }
    }

    /// The click target of the transcript row under `at`, if the transcript is
    /// what is on screen there. The geometry is the RENDERER's
    /// (`chat_height`/`chat_body_height`/`line_at_slot`), so a click cannot
    /// land one row off the row that was drawn.
    fn transcript_click_target(&self, at: Point) -> Option<String> {
        self.transcript_line_at(at).and_then(|l| l.click.clone())
    }

    /// The STYLED transcript row under `at`, if the transcript is what is on
    /// screen there.
    fn transcript_line_at(&self, at: Point) -> Option<crate::lines::VLine> {
        // The panel, the job view and the overlay DISPLACE the transcript;
        // a click in that region belongs to them.
        if self.help_open || self.panel.open() || self.job.is_some() {
            return None;
        }
        let chat_h = self.chat_height(self.cols.max(20), self.rows.max(8), self.popup_height());
        let y = u16::try_from(at.y).ok()?;
        if y < Self::CHAT_TOP || y >= Self::CHAT_TOP + chat_h {
            return None;
        }
        let lines = self.transcript_vlines();
        // `queued` is 0: this client holds no queued rows yet (the renderer
        // passes `queued: &[]`), and the two must agree or the hit-test slides.
        let body = crate::lines::chat_body_height(chat_h as usize, 0, self.notice.is_some());
        crate::lines::line_at_slot(&lines, body, self.scroll_off, (y - Self::CHAT_TOP) as usize)
            .cloned()
    }

    /// Act on a transcript row's click target.
    fn click_target(&mut self, target: &str) {
        // A branch card descends; it does not fold. Same route the rail's ⏎
        // takes.
        if let Some(id) = target.strip_prefix("open:") {
            self.transport.effect(Effect::OpenSession(id.to_string()));
            return;
        }
        // A job card opens that job's output — the same surface ⏎ reaches from
        // the rail, and the only route to a job that has already exited off it.
        if let Some(rest) = target.strip_prefix("job:") {
            let mut bits = rest.split(':');
            let (Some(_session), Some(job_id)) = (bits.next(), bits.next()) else {
                return;
            };
            if job_id.is_empty() {
                return;
            }
            self.job = Some(JobView {
                id: job_id.to_string(),
                output: String::new(),
                job: self
                    .jobs
                    .iter()
                    .find(|j| j.job.id == job_id)
                    .map(|r| r.job.clone()),
                error: None,
                scroll: 0,
                armed: false,
            });
            self.transport
                .effect(Effect::LoadJobOutput(job_id.to_string()));
            return;
        }
        // A workflow card opens that run's view. The run is detached and off
        // the live rail the moment it ends, so the card is the only door left
        // to its phases, its per-agent cost and its replay accounting.
        if let Some(id) = target.strip_prefix("workflow:") {
            self.panel.open_run(id);
            self.transport.effect(Effect::LoadWorkflow(id.to_string()));
            return;
        }
        // "+N more lines" lifts the cap and stays lifted — re-capping it is `^e`.
        if let Some(base) = target.strip_suffix("!full") {
            self.full_keys.insert(base.to_string());
            return;
        }
        if !self.open_keys.remove(target) {
            self.open_keys.insert(target.to_string());
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
            self.attachments.len(),
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
            // ↑/↓ walk the DRAFT's lines once there is more than one, and the
            // history ring otherwise — the guard the table already declares on
            // `cursor.up`/`cursor.down`. Left at the default `false` these two
            // rows could never fire, so a newline'd draft had no way up.
            multiline: self.draft.contains('\n'),
            // ← is `back to the session that spawned this one` and a cursor
            // move everywhere else. Left at the default `false` the guard never
            // held, so the binding the `?` overlay prints could not fire — and
            // a drilled-into subagent was a room with no door. The fact is the
            // meter's own: there is an origin to go back to.
            in_subagent: self.session.as_ref().is_some_and(|s| s.origin_id.is_some()),
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
                self.start_fresh_conversation();
                self.help_open = false;
                self.panel.state.open = false;
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
            // The scripts saved by name. Not session-scoped: saved workflows
            // live in `$BOUGH_HOME`, so this answers before the first turn too.
            Command::SavedShow => self.transport.effect(Effect::LoadSaved),
            // Both of the next two are ABOUT a conversation, so with none open
            // they say why rather than showing an empty list that reads as "you
            // have published nothing" / "no rules apply".
            Command::ArtifactsShow => {
                if self.session_id.is_none() {
                    self.notice = Some(NO_CONVERSATION_ARTIFACTS.to_string());
                    return;
                }
                self.transport.effect(Effect::LoadArtifacts);
            }
            Command::RulesShow => {
                if self.session_id.is_none() {
                    self.notice = Some(NO_CONVERSATION_RULES.to_string());
                    return;
                }
                self.transport.effect(Effect::LoadProjectRules);
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
                HostRequest::LoadThread(id) => self.transport.effect(Effect::LoadForeignThread(id)),
                HostRequest::LoadChildSessions(id) => {
                    self.transport.effect(Effect::LoadChildSessions(id))
                }
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
                // `^e`: every tool call and every thinking block at once. The
                // global toggle wins — flipping it drops the per-group state,
                // so `^e` twice is a reset rather than a return to whatever was
                // open before.
                Command::FoldAll => {
                    self.open_keys.clear();
                    self.full_keys.clear();
                    self.fold_all = !self.fold_all;
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
                // ^g: the open conversation's id, on the clipboard AND in the
                // notice. The id goes through the same OSC 52 path a drag uses,
                // so it survives ssh and tmux; saying it as well means a
                // terminal with no clipboard still leaves it selectable on
                // screen rather than the gesture silently doing nothing.
                Command::SessionCopyId => {
                    match self.session_id.clone() {
                        Some(id) => {
                            (self.copy)(&id);
                            self.notice = Some(format!("copied {id}"));
                        }
                        None => self.notice = Some(NO_CONVERSATION_TO_COPY.to_string()),
                    }
                    true
                }
                // The take-back window's Escape. The keymap decided this
                // outranks the stop; this arm is only the gesture.
                Command::MessageUnsend => {
                    self.take_back();
                    true
                }
                // ^n. The `?` overlay has promised "start a fresh conversation"
                // since the overlay was generated from the keymap, and the
                // chord resolved to a command NOTHING answered — so the one key
                // the help sheet names for leaving a thread did nothing at all.
                // `/new` reaches the same body; this is the chord for it.
                Command::SessionNew => {
                    self.run_client_command(Command::SessionNew, "");
                    true
                }
                // ←. THE DOOR OUT OF A DRILLED-INTO AGENT. The `?` overlay has
                // printed "back to the session that spawned this one" all
                // along, and nothing answered the command — you could ⏎ into a
                // subagent from the rail and then only leave it through the
                // switcher. The guard (`in_subagent`) is what keeps this off
                // the cursor's ←, and the origin is the same one the `← back`
                // chip is drawn from, so chip, binding and destination cannot
                // disagree.
                Command::SessionOut => {
                    if let Some(origin) = self.session.as_ref().and_then(|s| s.origin_id.clone()) {
                        self.transport.effect(Effect::OpenSession(origin));
                    }
                    true
                }
                // ↑/↓ recall. The keymap guards these: ↑ is a CURSOR move in a
                // multi-line draft and an attachment walk when one is queued,
                // so by the time it resolves to `HistoryPrev` recall is
                // unambiguously what was meant.
                Command::HistoryPrev => {
                    self.history_prev();
                    true
                }
                Command::HistoryNext => {
                    self.history_next();
                    true
                }
                // ^v: the pasteboard's PICTURE first, its text second
                // (`clipboard.rs`). The read and the upload are the transport's;
                // what comes back is an attachment or a paste.
                Command::ImagePaste => {
                    self.transport.effect(Effect::ImagePaste);
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
                            job: self
                                .jobs
                                .iter()
                                .find(|j| j.job.id == unit.id)
                                .map(|r| r.job.clone()),
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
            // A MESSAGE STILL WEARING ITS OPTIMISTIC ID HAS NO SERVER ID YET,
            // and this is the documented gesture's own timing: esc immediately
            // after Enter is exactly the moment before the snapshot comes back
            // and renames `local-3` to the row the server wrote. Sending that
            // name to the unsend route made the server answer "message local-3
            // is not one of this session's own messages, so it cannot be
            // unsent" — a refusal the user reads as the feature being broken,
            // in the one usage the help sheet describes. The id is resolved
            // against the server instead (`Effect::UnsendLatest`).
            crate::forest::TakeBack::Sent {
                at_message_id,
                text,
            } if at_message_id.starts_with(LOCAL_ID_PREFIX) => {
                self.transport.effect(Effect::UnsendLatest { text })
            }
            crate::forest::TakeBack::Sent { at_message_id, .. } => {
                self.transport.effect(Effect::Unsend(at_message_id))
            }
            // This client has no queue (nothing is held while busy), so a
            // queued take-back cannot arise; `None` is the honest no-op.
            crate::forest::TakeBack::Queued | crate::forest::TakeBack::None => {}
        }
    }

    /// Disarm the quit AND retract what it said. Clearing the flag alone left
    /// `^c again to quit` on screen over a confirm that no longer existed —
    /// a promise the next `^c` would not keep.
    fn disarm_quit(&mut self) {
        self.quit_armed = false;
        if self.notice.as_deref() == Some(QUIT_CONFIRM) {
            self.clear_notice();
        }
    }

    fn on_key(&mut self, k: KeyEvent, now_ms: i64) {
        if k.kind == KeyEventKind::Release {
            return;
        }
        if self.on_surface_key(&k) {
            // Any chord other than ^c disarms the quit (App.tsx).
            self.disarm_quit();
            return;
        }
        if self.on_completion_key(&k) {
            self.disarm_quit();
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        // Any chord other than ^c disarms the quit (App.tsx).
        let is_ctrl_c = ctrl && matches!(k.code, KeyCode::Char('c'));
        if !is_ctrl_c {
            self.disarm_quit();
        }
        // THE LINE EDITOR IS keys.rs'S, not a second one grown here. The chords
        // the table already declares — ⌥b/⌥f, ^w, ⌥⌫, ^k/^u, ↑/↓ in a multi-line
        // draft — resolve to `Command::Cursor*`/`Command::Delete*` and were
        // dropped on the floor by the raw `KeyCode` match below, which only ever
        // knew about ^a/^e/home/end/backspace/←/→. Routing through
        // `keys::edit_line` means one editor, the tested one.
        if let Some(command) = self.resolve(&k) {
            if self.on_edit_command(command) {
                return;
            }
        }
        match (k.code, ctrl) {
            (KeyCode::Char('c'), true) => {
                if self.quit_armed {
                    self.quit = true;
                } else {
                    self.quit_armed = true;
                    self.notice = Some(QUIT_CONFIRM.to_string());
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

    // ---- line editing (keys.rs's editor; cursor is a char index) -----------

    /// The composer's draft as the pure editor sees it, and back again.
    ///
    /// `keys::edit_line` holds the WHOLE line editor — word motion, the kills,
    /// the multi-line up/down — and holds it as a tested pure function. This is
    /// the adapter, and it is the only thing this file needs to own: the draft
    /// and the cursor go in, a new `LineState` comes out.
    ///
    /// Returns false for any command that is not an edit, so the caller falls
    /// through to the chords that are NOT the line editor's (send, escape, the
    /// scrolls).
    fn on_edit_command(&mut self, command: Command) -> bool {
        use crate::keys::edit_line;
        // The editing subset, named exhaustively rather than inferred: a new
        // command must be added here deliberately, not silently swallowed by a
        // catch-all that would eat `Command::SessionNew` as a no-op edit.
        if !matches!(
            command,
            Command::CursorLeft
                | Command::CursorRight
                | Command::CursorHome
                | Command::CursorEnd
                | Command::CursorWordLeft
                | Command::CursorWordRight
                | Command::CursorUp
                | Command::CursorDown
                | Command::DeleteBack
                | Command::DeleteForward
                | Command::DeleteWordBack
                | Command::DeleteToEnd
                | Command::DeleteToStart
                | Command::DeleteLine
        ) {
            return false;
        }
        let before = crate::keys::LineState {
            text: std::mem::take(&mut self.draft),
            cursor: self.cursor,
        };
        let after = edit_line(&before, command);
        let changed_text = after.text != before.text;
        self.draft = after.text;
        self.cursor = after.cursor.min(self.draft.chars().count());
        // Only a TEXT change can change what completes; a bare cursor move must
        // not re-open a popup an earlier escape dismissed — nor move its cursor.
        if changed_text {
            self.completion_sel = 0;
            self.ensure_candidates();
        }
        true
    }

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
        // …and it NARROWS the popup, which is a different list. See
        // `completion_sel`: without this, row 4 of five becomes row 2 of two by
        // clamping, and ⏎ runs whatever the clamp landed on.
        self.completion_sel = 0;
        self.ensure_candidates();
    }

    /// A whole string at the cursor, as one edit. Used by every paste path.
    fn insert_text(&mut self, text: &str) {
        let at = self.byte_at(self.cursor);
        self.draft.insert_str(at, text);
        self.cursor += text.chars().count();
        self.dismissed = false;
        self.completion_sel = 0;
        self.ensure_candidates();
    }

    // ---- paste --------------------------------------------------------------

    /// A bracketed paste off the terminal.
    ///
    /// A PASTED PATH TO AN IMAGE IS A PICTURE, and this is the path ⌘v actually
    /// takes in most terminals: the terminal keeps the keypress and hands over
    /// the clipboard's TEXT, which for a file copied in Finder is its path. The
    /// decision is pure and never touches disk (`clipboard::clipboard_image_path`),
    /// so an ordinary paste pays one regex and falls straight through.
    fn on_paste(&mut self, text: &str) {
        // Only where a draft is being typed: a burst inserted into a composer
        // hidden under the panel or the help overlay is text nobody can see.
        if self.ui_mode() != UiMode::Chat {
            return;
        }
        let clean = strip_ctl(text);
        if clean.is_empty() {
            return;
        }
        if clipboard_image_path(&clean).is_some() {
            self.transport.effect(Effect::AttachPath(clean));
            return;
        }
        self.on_paste_text(&clean);
    }

    /// Pasted WORDS, wherever they came from: inlined when short, and otherwise
    /// held aside with a `[Pasted text #N]` mark left where the cursor was.
    /// The mark is the record — deleting it drops the paste (`paste.rs`).
    fn on_paste_text(&mut self, text: &str) {
        let clean = strip_ctl(text);
        if clean.is_empty() {
            return;
        }
        if clean.chars().count() <= QUEUE_ABOVE_CHARS {
            self.insert_text(&clean);
            return;
        }
        self.pastes.push(clean);
        let mark = paste_mark(self.pastes.len());
        self.insert_text(&mark);
    }

    fn delete_back(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let at = self.byte_at(self.cursor - 1);
        self.draft.remove(at);
        self.cursor -= 1;
        self.completion_sel = 0;
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
        // A recall that is thrown away is over: the next ↑ starts from the
        // newest line again rather than resuming where the last walk stopped.
        self.hist_at = None;
    }

    /// The line just sent, on the ↑ ring. Verbatim, and never twice in a row —
    /// re-sending the same thing five times should not make ↑ walk it five
    /// times before reaching what came before it.
    fn remember_sent(&mut self, text: &str) {
        if self.sent_history.last().map(String::as_str) == Some(text) {
            self.hist_at = None;
            return;
        }
        self.sent_history.push(text.to_string());
        self.hist_at = None;
    }

    /// ↑: one line back through what this screen has sent. From a draft that is
    /// NOT empty the keymap has already resolved ↑ to a cursor move, so this
    /// only ever runs where recall is the thing the user meant.
    fn history_prev(&mut self) {
        if self.sent_history.is_empty() {
            return;
        }
        let at = match self.hist_at {
            None => self.sent_history.len() - 1,
            Some(i) => i.saturating_sub(1),
        };
        self.hist_at = Some(at);
        self.draft = self.sent_history[at].clone();
        self.cursor = self.draft.chars().count();
        self.completion_sel = 0;
    }

    /// ↓: forward again, and off the end back to the empty draft you started
    /// from — not to the oldest line, which would make the ring a loop with no
    /// way out.
    fn history_next(&mut self) {
        let Some(i) = self.hist_at else { return };
        let at = i + 1;
        if at >= self.sent_history.len() {
            self.hist_at = None;
            self.draft.clear();
            self.cursor = 0;
            return;
        }
        self.hist_at = Some(at);
        self.draft = self.sent_history[at].clone();
        self.cursor = self.draft.chars().count();
        self.completion_sel = 0;
    }

    /// Everything this screen was showing FOR the conversation being left.
    /// `^n`'s whole body, and what a `!`-borrowed shell session is dropped
    /// with when an ordinary message is sent (see `submit`).
    fn start_fresh_conversation(&mut self) {
        self.scroll_off = 0;
        // The composer's QUEUE goes with the conversation it was being written
        // for. An image queued for one thread and silently sent to the next is
        // the composer keeping something the user cannot see it keeping.
        self.attachments.clear();
        self.pastes.clear();
        self.session_id = None;
        self.shell_session = false;
        self.panel.current_id = None;
        self.thread.clear();
        self.streaming.clear();
        self.tool_logs.clear();
        self.marks.clear();
        self.turn = None;
        self.activity = None;
        self.clear_notice();
        self.last_send_at = None;
        // …and every fact the STATUS BAR was stating about the conversation
        // being left. `SessionOpened` already clears exactly these on a switch;
        // `/new` did not, so a fresh screen kept the old thread's `$cost`,
        // `% ctx left` and `← back` — a bar describing a conversation that is
        // no longer on it. Same list, same reason: re-read, never carried.
        self.session = None;
        self.usage = None;
        self.effective_model = None;
        self.context_limit = None;
        self.primed_tags.clear();
        self.project_rules.clear();
        // Another conversation's shells and holds pinned under this composer
        // would be a claim about work this screen is not doing.
        self.jobs.clear();
        self.rail_sel = None;
        self.rail_armed = None;
        self.job = None;
        self.ask = None;
        self.ask_typed.clear();
        // The transport is reusing a session id of its own; without this the
        // next send would land back in the old conversation.
        self.transport.effect(Effect::NewConversation);
    }

    fn page(&self) -> usize {
        (self.rows as usize).saturating_sub(8).max(1)
    }

    // ---- submit -------------------------------------------------------------

    fn submit(&mut self) {
        let text = self.draft.clone();
        // An image with nothing said about it is still a message: the picture
        // IS the question often enough that refusing it would be the bug.
        if text.is_empty() && self.attachments.is_empty() {
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
            // In the ↑ history WITH the sigil, so re-running the last command
            // is ↑⏎ — the thing a shell user does constantly. One ring, kept
            // verbatim: one history is one mental model, and `!` is visible in
            // it.
            self.remember_sent(&text);
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
            self.remember_sent(&text);
            self.transport.effect(Effect::Run(command, arg));
            return;
        }
        // THE `shell` CONVERSATION IS NOT ONE YOU CHAT IN. `!echo hi` as the
        // first thing typed on a fresh screen borrows (or creates) the
        // workspace's one shell conversation so the job has somewhere to live —
        // and this screen then WAS that conversation, so every message after it
        // landed in a thread permanently titled "shell" and typed `kind:shell`.
        // A message is an ordinary conversation's business, so it starts one.
        if self.shell_session {
            self.start_fresh_conversation();
        }
        self.clear_draft();
        self.scroll_off = 0;
        self.remember_sent(&text);
        // Marks expand WHERE THEY SIT, so the message reads the way the draft
        // did; a paste whose mark was deleted is dropped (`paste.rs`).
        let message = expand_pastes(&text, &self.pastes);
        let images = std::mem::take(&mut self.attachments);
        self.pastes.clear();
        // The take-back window opens here, and it SAYS so: three seconds that
        // nothing announces is a gesture only the keymap knows about.
        self.last_send_at = Some(self.now_ms);
        self.notice = Some(TAKE_BACK_HINT.to_string());
        // Optimistic local echo; the snapshot/SSE merge reconciles by id later.
        self.sent_seq += 1;
        let local_id = format!("{LOCAL_ID_PREFIX}{}", self.sent_seq);
        self.thread.push(Message {
            id: local_id.clone(),
            session_id: String::new(),
            role: Role::User,
            parts: vec![Part::Text {
                text: message.clone(),
            }],
            pending: false,
            created_at: self.now_ms,
        });
        self.transport.effect(Effect::Send {
            text: message,
            images,
            local_id,
        });
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
            // A running program's own output, line by line. Kept per CALL —
            // `build_lines` shows it under a call that has no result yet and
            // drops it the moment the finalized `tool_result` arrives, so this
            // never double-prints.
            EventType::ToolLog => {
                if let Ok(d) =
                    serde_json::from_value::<bough_core::schema::events::ToolLogData>(event.data)
                {
                    self.tool_logs.entry(d.call_id).or_default().push(d.line);
                }
            }
            EventType::TurnFinished => {
                if let Ok(d) = serde_json::from_value::<TurnFinishedData>(event.data) {
                    if let Some(turn) = &mut self.turn {
                        turn.ended = true;
                    }
                    // The settle line, into the ledger the transcript
                    // interleaves (reduce.rs::TurnSettle). It is a MARK and not
                    // a message: the server never stored it, and a turn that
                    // ended is a fact the transcript must not lose when the
                    // spinner's numbers go.
                    if let Some(started_at) = self.turn.as_ref().map(|t| t.started_at) {
                        let meter = crate::store::state::TurnMeter {
                            session_id: d.session_id.clone(),
                            started_at,
                            base_tokens: 0,
                            base_cost_usd: 0.0,
                            tokens: 0,
                            cost_usd: 0.0,
                            ended_at: Some(event.ts),
                            status: Some(d.status),
                        };
                        self.marks.push(crate::store::state::TranscriptMark {
                            id: format!("mark:{}:{started_at}", d.session_id),
                            session_id: d.session_id,
                            at: event.ts,
                            kind: crate::store::state::MarkKind::Turn,
                            text: crate::store::selectors::settled_line(&meter, event.ts),
                        });
                    }
                    for msg in &mut self.thread {
                        msg.pending = false;
                    }
                    self.activity = None;
                    // The settle: the round that just ended is the one that
                    // moved the context and the spend, and the rules and tags
                    // are re-read from disk per turn — so the meter's numbers
                    // are FINAL only after this read, not before it.
                    self.transport.effect(Effect::LoadSessionMeta);
                    self.transport.effect(Effect::PollUsage);
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

    // ---- the transcript ------------------------------------------------------

    /// Is this group open? The global toggle wins over the per-group set, the
    /// way App.tsx passes `(key) => foldAll || openKeys.has(key)` to
    /// `buildLines`.
    fn is_expanded(&self, key: &str) -> bool {
        self.fold_all || self.open_keys.contains(key)
    }
    /// Has this block's line cap been lifted?
    fn is_full(&self, key: &str) -> bool {
        self.fold_all || self.full_keys.contains(key)
    }

    /// The transcript as PLAIN rows — the styled rows with their SGR gone.
    /// Only tests, the scroll clamp and the copy path want this: the RENDERER
    /// takes `transcript_vlines` and parses the escapes (`components/chat.rs`),
    /// so there is one derivation and a click cannot land on a row that was
    /// not drawn.
    fn transcript_lines(&self) -> Vec<String> {
        self.transcript_vlines()
            .into_iter()
            .map(|l| crate::ansi::strip_ansi(&l.text))
            .collect()
    }

    /// THE REAL TRANSCRIPT. `lines::build_lines` is the full port — tool folds
    /// and their caps, thinking, live `tool.log` rows, branch cards, job cards,
    /// workflow cards, the `#` margin rows, marks — and this is its only
    /// caller. The options are assembled from state this reducer already holds;
    /// three of them are adapted here rather than in `lines.rs`, because the
    /// same wire shape is declared twice in this crate (`api` = what the wire
    /// carries, `store::state` = what the ported selectors take). The field
    /// lists are written out, so a divergence is a compile error here.
    fn transcript_vlines(&self) -> Vec<crate::lines::VLine> {
        let width = self.cols.max(20) as usize;
        // Delegated children of THIS conversation, with their completion notes
        // matched out of the thread: the branch-card feed.
        let children: Vec<crate::lines::ChildRow> = match self.session_id.as_deref() {
            Some(current) => self
                .panel
                .sessions
                .iter()
                .filter(|s| {
                    s.session.parent_id.as_deref() == Some(current)
                        || s.session.origin_id.as_deref() == Some(current)
                })
                .map(|s| crate::lines::ChildRow {
                    id: s.session.id.clone(),
                    title: s.session.title.clone(),
                    kind: s.session.kind,
                    busy: s.busy,
                    last_turn_status: s.last_turn_status,
                    outcome_ok: s.session.outcome_ok,
                    origin_message_id: s.session.origin_message_id.clone(),
                    tokens: s.tokens,
                    cost_usd: s.cost_usd,
                })
                .collect(),
            None => Vec::new(),
        };
        // A RUNNING shell is on the rail, so its card in the transcript would
        // be the same fact twice. An exited one stays: an outcome belongs in
        // the transcript, and the card is the only door left to its output.
        let jobs: Vec<crate::lines::JobView> = self
            .jobs
            .iter()
            .filter(|r| r.job.status != bough_core::schema::parts::JobStatus::Running)
            .map(|r| crate::lines::JobView {
                job: r.job.clone(),
                tail: r.tail.clone().unwrap_or_default(),
                output_lines: r.output_lines.unwrap_or(0),
            })
            .collect();
        // Every run of this session, not just the live ones: the card's whole
        // purpose is that a finished run still reads its outcome in place.
        let runs: Vec<crate::lines::RunCardView> = self
            .workflows
            .iter()
            .map(|w| crate::lines::RunCardView {
                id: w.id.clone(),
                status: workflow_status(&w.status),
                agents: crate::store::state::WorkflowAgentCounts {
                    total: w.agents.total as i64,
                    done: w.agents.done as i64,
                    cached: w.agents.cached as i64,
                    running: w.agents.running as i64,
                    queued: w.agents.queued as i64,
                    failed: w.agents.failed as i64,
                },
                created_at: w.created_at,
                finished_at: w.finished_at,
            })
            .collect();
        let opts = crate::lines::BuildOptions {
            streaming: self.streaming.clone(),
            branches: crate::lines::branches_from(&self.thread, &children),
            tool_logs: Some(self.tool_logs.clone()),
            jobs,
            runs,
            marks: self.marks.clone(),
            skills: (!self.skills.is_empty())
                .then(|| self.skills.iter().map(|(n, _)| n.clone()).collect()),
            now: Some(self.now_ms),
            primed_tags: self.primed_tags.clone(),
            project_rules: self.project_rules.clone(),
        };
        crate::lines::build_lines(
            &self.thread,
            &|key| self.is_expanded(key),
            &|key| self.is_full(key),
            width,
            &opts,
        )
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

    /// The frame: every surface, then the selection painted over all of them.
    /// The fetches that must happen before the first frame rather than on the
    /// tab that consumes them. The stored palette is one: a theme applied only
    /// when the picker is opened is a theme that never paints.
    pub fn boot(&mut self) {
        self.transport.effect(Effect::LoadTheme);
    }

    pub fn draw(&self, area: Rect, buf: &mut Buffer) {
        // The screen background (theme.rs's third paint path). Every surface
        // paints its own, but the gaps between them are the root box — without
        // this a "deeper surfaces" preset recoloured the panels and left the
        // space around them on the terminal's own background.
        buf.set_style(area, Style::default().bg(crate::theme::colors().bg));
        self.draw_surfaces(area, buf);
        self.paint_selection(area, buf);
    }

    /// The selection, drawn as ONE layer over whatever is underneath
    /// (App.tsx::SelectionLayer).
    ///
    /// A drag is a gesture in SCREEN coordinates, which is why this reads the
    /// finished buffer rather than being threaded through each component: the
    /// first cut of the TS painted through the transcript's own decorate hook,
    /// and the panel, the rail and the composer could then be dragged over and
    /// copied while showing no highlight at all. One mechanism, addressed the
    /// way the gesture already is.
    ///
    /// EXPLICIT COLOURS, not `Modifier::REVERSED`. Inverting needs something to
    /// invert, and transcript cells that never set a background of their own
    /// resolve both sides to the same colour — the TS shipped exactly that bug
    /// and the cells reported `inverse: true` the whole time, so the attribute
    /// being set was never proof the highlight was visible.
    fn paint_selection(&self, area: Rect, buf: &mut Buffer) {
        let Some(sel) = self.sel else { return };
        if is_empty_selection(&sel) {
            return;
        }
        let palette = crate::theme::palette();
        let style = Style::default()
            .fg(palette.bg_color())
            .bg(palette.accent_color());
        let (top, bottom) = crate::selection::sel_rows(&sel);
        for y in top..=bottom {
            // Selection rows are 1-based cells; the buffer is 0-based.
            let Some(row) = u16::try_from(y - 1).ok().filter(|r| *r < area.height) else {
                continue;
            };
            let Some(span) = crate::selection::row_span(&sel, y) else {
                continue;
            };
            let from = span.from.min(area.width as usize);
            let to = if span.to == crate::selection::EOL {
                area.width as usize
            } else {
                span.to.min(area.width as usize)
            };
            // A drag past end-of-line selects nothing on that row. Painting it
            // anyway would hang a bar of accent off the end of short lines.
            let blank = (from..to).all(|x| buf[(area.x + x as u16, area.y + row)].symbol() == " ");
            if blank {
                continue;
            }
            for x in from..to {
                buf[(area.x + x as u16, area.y + row)].set_style(style);
            }
        }
    }

    fn draw_surfaces(&self, area: Rect, buf: &mut Buffer) {
        let cols = area.width.max(20);
        let rows = area.height.max(8);
        // The overlay is the one surface that displaces everything, header
        // and composer included.
        if self.help_open {
            render_help(rows as usize, self.help_off, area, buf);
            return;
        }
        let lines = self.transcript_vlines();
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
            header.push(Span::styled(
                "  · disconnected",
                Style::default().fg(warn()),
            ));
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
            // The queued images, by the names the server gave them. Only images
            // get a row: a held paste's row IS its `[Pasted text #N]` mark in
            // the draft, and drawing the same label underneath would be the
            // same thing said twice (`paste.rs`).
            let attachment_names: Vec<String> =
                self.attachments.iter().map(|a| a.name.clone()).collect();
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
                    attachments: &attachment_names,
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
            &self.meter(),
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
    // TWO CROSSTERMS. `ratatui::init()` enables raw mode through the crossterm
    // ratatui itself depends on (0.28); the `EventStream` below — the thing that
    // actually PARSES the bytes — is the 0.29 this crate depends on, and
    // `is_raw_mode_enabled()` is a per-crate global. 0.29 therefore believed the
    // terminal was still cooked and decoded `\n` (0x0a) as Enter rather than as
    // Ctrl+J, which is the one byte those two keys disagree about: `^j` SENT THE
    // MESSAGE instead of inserting a newline, so a multi-line draft could not be
    // typed at all and every chord that only means something in one — ↑/↓ across
    // lines — was unreachable no matter how correctly it was wired.
    //
    // Enabling it again on 0.29's side is the fix and it is not a double-enable:
    // each crate stores the prior termios once, and the terminal is already in
    // the state this asks for.
    let _ = crossterm::terminal::enable_raw_mode();
    // Wheel scroll is the one mouse gesture wave 1 ships.
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    // Focus reporting: `notify_desktop` is silent while focused, and it can
    // only know that if the terminal says so.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableFocusChange);
    // BRACKETED PASTE — the mode that makes a paste ONE event instead of N
    // keystrokes. Without it a real terminal delivers pasted text as ordinary
    // key presses, so every newline in it is Enter: a multi-line paste SENDS,
    // line by line, and `Event::Paste` never arrives at all no matter how
    // correctly the handler is wired.
    //
    // `input.rs::enter_tui` sets this (and mouse, and focus) in one sequence
    // and is dead code — nothing ever called it, which is exactly how this
    // went missing while its two neighbours above were enabled by hand. The
    // modes are set here, next to the others, rather than by resurrecting that
    // function: two places that both claim to own terminal mode is how they
    // drift apart in the first place.
    //
    // NOT catchable by the shell-use suite: that harness writes whatever bytes
    // the test asks for, including `\e[200~…\e[201~`, whether or not the
    // program ever requested the mode — so paste "passed" against a terminal
    // no user has.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste);
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
    app.boot();

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
        // Was there something moving BEFORE this action? The tick that RETIRES
        // the last moving thing must still be drawn, or the screen keeps
        // painting what the state no longer holds: an expired notice cleared
        // itself, `animating()` went false on the same tick, the draw was
        // skipped — and the row stayed on screen forever anyway.
        let was_animating = app.animating();
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
        if is_tick && !app.animating() && !was_animating {
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
    // Paste mode off with the rest: left on, the shell that follows receives
    // its own pastes wrapped in `\e[200~` markers it will happily type out.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
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
            Effect::Send {
                text,
                images,
                local_id,
            } => {
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
                                    let _ = tx.send(Action::SendFailed {
                                        local_id,
                                        text: text.clone(),
                                    });
                                    return;
                                }
                            }
                        }
                    };
                    let body = bough_core::schema::requests::PostMessageBody {
                        text: text.clone(),
                        images: (!images.is_empty()).then_some(images),
                    };
                    if let Err(e) = api.post_message(&sid, &body).await {
                        let _ = tx.send(Action::Notice(e.to_string()));
                        let _ = tx.send(Action::SendFailed { local_id, text });
                    }
                });
            }
            // ⌃v. The pasteboard's image data outranks its text, because a
            // pasteboard holding a picture almost always ALSO holds a string
            // (`clipboard.rs`) — and a picture read as its filename put prose
            // about an unopenable file in front of the model.
            Effect::ImagePaste => {
                tokio::spawn(async move {
                    match crate::clipboard::paste_clipboard().await {
                        None => {
                            let _ = tx.send(Action::Notice(CLIPBOARD_EMPTY.to_string()));
                        }
                        Some(crate::clipboard::Clipboard::Text(text)) => {
                            let _ = tx.send(Action::PasteText(text));
                        }
                        Some(crate::clipboard::Clipboard::Image { bytes, media_type }) => {
                            match api.upload_image(bytes, &media_type).await {
                                Ok(part) => {
                                    let _ = tx.send(Action::Attached(as_image(part)));
                                }
                                Err(e) => {
                                    let _ = tx.send(Action::Notice(e.to_string()));
                                }
                            }
                        }
                    }
                });
            }
            // A pasted PATH to an image. A read that fails is a text paste:
            // the string may be a path the user meant to type, and swallowing
            // it would be worse than not attaching.
            Effect::AttachPath(text) => {
                tokio::spawn(async move {
                    match crate::clipboard::clipboard_from_text(&text).await {
                        crate::clipboard::Clipboard::Image { bytes, media_type } => {
                            match api.upload_image(bytes, &media_type).await {
                                Ok(part) => {
                                    let _ = tx.send(Action::Attached(as_image(part)));
                                }
                                // The bytes are unusable, but the string is
                                // still what the user pasted.
                                Err(e) => {
                                    let _ = tx.send(Action::Notice(e.to_string()));
                                    let _ = tx.send(Action::PasteText(text));
                                }
                            }
                        }
                        crate::clipboard::Clipboard::Text(text) => {
                            let _ = tx.send(Action::PasteText(text));
                        }
                    }
                });
            }
            // The take-back whose message is still wearing `local-N`. The id is
            // the SERVER's to give, so it is read back — and the text is
            // compared before anything is deleted, because a POST still in
            // flight would otherwise make this retract the message before it.
            Effect::UnsendLatest { text } => {
                tokio::spawn(async move {
                    let Some(sid) = session.lock().expect("session lock").clone() else {
                        return;
                    };
                    let snapshot = match api.get_session(&sid).await {
                        Ok(snapshot) => snapshot,
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                            return;
                        }
                    };
                    let latest = snapshot.thread.iter().rev().find(|m| m.role == Role::User);
                    let Some(target) = latest.filter(|m| crate::forest::message_text(m) == text)
                    else {
                        let _ = tx.send(Action::Notice(TAKE_BACK_TOO_SOON.to_string()));
                        return;
                    };
                    match api.unsend(&sid, &target.id).await {
                        Ok(result) => {
                            let _ = tx.send(Action::TookBack(result.text));
                            if let Ok(snapshot) = api.get_session(&sid).await {
                                let _ = tx.send(Action::Thread(snapshot.thread));
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
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
            Effect::LoadChildSessions(origin_id) => {
                tokio::spawn(async move {
                    // A failed drill-in stays silent: the plain listing is
                    // already on screen and a notice per poll would be noise.
                    if let Ok(rows) = api.list_sessions(Some(&origin_id)).await {
                        let _ = tx.send(Action::ChildSessions { origin_id, rows });
                    }
                });
            }
            Effect::ReloadThread => {
                tokio::spawn(async move {
                    let Some(sid) = session.lock().expect("session lock").clone() else {
                        return;
                    };
                    if let Ok(snap) = api.get_session(&sid).await {
                        let _ = tx.send(Action::Thread(snap.thread));
                    }
                });
            }
            Effect::LoadForeignThread(session_id) => {
                tokio::spawn(async move {
                    if let Ok(snap) = api.get_session(&session_id).await {
                        let _ = tx.send(Action::ForeignThread {
                            session_id,
                            thread: snap.thread,
                        });
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
                        let _ = tx.send(Action::Jobs(list.jobs));
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
                                let _ = tx.send(Action::Jobs(list.jobs));
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
                    // …and the SESSION's own meta beside it, because that is
                    // where `effective_model` comes from. Re-reading only the
                    // install-wide settings left the status bar naming the old
                    // model until the next restart — the one surface that is
                    // supposed to confirm the pin took.
                    let open = session.lock().expect("session lock").clone();
                    if let Some(sid) = open {
                        if let Ok(s) = api.get_session(&sid).await {
                            let _ = tx.send(Action::SessionMeta(Box::new(SessionMeta {
                                session: s.session,
                                usage: s.usage,
                                effective_model: s.effective_model,
                                context_limit: s.context_limit,
                                primed_tags: s.primed_tags.unwrap_or_default(),
                                project_rules: s.project_rules.unwrap_or_default(),
                            })));
                        }
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
                                    // BORROWED, NOT ADOPTED. The screen shows
                                    // this conversation so the job's rail row
                                    // and output are readable — but it is the
                                    // workspace's `shell`, and the next
                                    // ordinary message must not be typed into
                                    // it (`App::submit`).
                                    let _ = tx.send(Action::ShellSessionOpened(id.clone()));
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
                        let _ = tx.send(Action::Jobs(list.jobs));
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
            // The three one-shot listings. Each was ASKED for out loud, so
            // unlike the rail's feeds a failure is said rather than swallowed:
            // silence after `/saved` is indistinguishable from "none".
            Effect::LoadSaved => {
                tokio::spawn(async move {
                    match api.list_saved_workflows().await {
                        Ok(rows) => {
                            let _ = tx.send(Action::Saved(rows));
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                });
            }
            Effect::LoadArtifacts => {
                tokio::spawn(async move {
                    let Some(sid) = session.lock().expect("session lock").clone() else {
                        let _ = tx.send(Action::Notice(NO_CONVERSATION_ARTIFACTS.to_string()));
                        return;
                    };
                    match api.list_artifacts(&sid).await {
                        Ok(rows) => {
                            let _ = tx.send(Action::Artifacts(rows));
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                });
            }
            Effect::LoadProjectRules => {
                tokio::spawn(async move {
                    let Some(sid) = session.lock().expect("session lock").clone() else {
                        let _ = tx.send(Action::Notice(NO_CONVERSATION_RULES.to_string()));
                        return;
                    };
                    // The whole snapshot, for one field: `projectRules` has no
                    // route of its own, and the files are re-read per turn
                    // server-side, so this IS the fresh answer.
                    match api.get_session(&sid).await {
                        Ok(snapshot) => {
                            let _ = tx.send(Action::ProjectRules(
                                snapshot.project_rules.unwrap_or_default(),
                            ));
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notice(e.to_string()));
                        }
                    }
                });
            }
            // The three meter feeds. Every one of them is SILENT on failure:
            // they run on a timer, and a banner per poll would turn a missing
            // number into a wall of banners. A stale meter says less; a modal
            // every second says nothing at all.
            Effect::LoadSessionMeta => {
                tokio::spawn(async move {
                    let Some(sid) = session.lock().expect("session lock").clone() else {
                        return;
                    };
                    if let Ok(s) = api.get_session(&sid).await {
                        let _ = tx.send(Action::SessionMeta(Box::new(SessionMeta {
                            session: s.session,
                            usage: s.usage,
                            effective_model: s.effective_model,
                            context_limit: s.context_limit,
                            primed_tags: s.primed_tags.unwrap_or_default(),
                            project_rules: s.project_rules.unwrap_or_default(),
                        })));
                    }
                });
            }
            Effect::PollUsage => {
                tokio::spawn(async move {
                    let Some(sid) = session.lock().expect("session lock").clone() else {
                        return;
                    };
                    if let Ok(u) = api.session_usage(&sid).await {
                        let _ = tx.send(Action::Usage(crate::api::SnapshotUsage {
                            totals: u.usage,
                            tree: u.tree,
                        }));
                    }
                });
            }
            Effect::LoadBranch(dir) => {
                tokio::spawn(async move {
                    // Not a repo, or no server: the meter simply says less.
                    if let Ok(b) = api.branch(&dir).await {
                        let _ = tx.send(Action::Branch {
                            dir,
                            branch: b.branch,
                        });
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
    /// A text-only send, which is every send in these tests: images are the
    /// ⌃v path and have their own.
    fn a_send(text: String) -> Effect {
        Effect::Send {
            text,
            images: Vec::new(),
            // Normalized away by `sends` — the echo's id is bookkeeping, and a
            // test about WHAT was sent must not be about how many sends
            // preceded it.
            local_id: String::new(),
        }
    }

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
                        // …and so is everything the STATUS BAR is made of: a
                        // meter feed says nothing about what was asked for.
                        | Effect::LoadSessionMeta
                        | Effect::PollUsage
                        | Effect::LoadBranch(_)
                        | Effect::LoadModelSettings
                )
            })
            .cloned()
            .map(|e| match e {
                Effect::Send { text, images, .. } => Effect::Send {
                    text,
                    images,
                    local_id: String::new(),
                },
                other => other,
            })
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

    fn meta_key(code: KeyCode) -> Action {
        Action::Term(TermEvent::Key(KeyEvent::new(code, KeyModifiers::ALT)))
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

    // ---- the last four client verbs: /saved, /artifacts, /rules, ^g --------

    fn saved(name: &str) -> bough_core::workflow::saved::SavedWorkflow {
        bough_core::workflow::saved::SavedWorkflow {
            name: name.into(),
            path: format!("/tmp/{name}.ts"),
            description: String::new(),
            bytes: 10,
            updated_at: 0,
        }
    }

    fn artifact(name: &str, id: &str) -> bough_core::hostfn::artifact::Artifact {
        bough_core::hostfn::artifact::Artifact {
            name: name.into(),
            url: format!("/artifacts/{id}/{name}"),
            href: format!("http://127.0.0.1:4325/artifacts/{id}/{name}"),
            bytes: 512,
            ts: 1,
        }
    }

    fn rule(path: &str, bytes: i64) -> crate::api::ProjectRuleSummary {
        crate::api::ProjectRuleSummary {
            label: path.into(),
            path: path.into(),
            bytes,
        }
    }

    /// `/saved` asks the server and says what came back. The empty sentence
    /// names the ONE gesture that makes a saved workflow; the full one names
    /// re-running, because no host function runs one by name.
    #[test]
    fn saved_lists_the_scripts_and_names_a_gesture_that_exists() {
        assert_eq!(
            describe_saved_workflows(&[]),
            "no saved workflows — open a run in ^w and press s to save its script"
        );
        assert_eq!(
            describe_saved_workflows(&[saved("nightly"), saved("triage")]),
            "2 saved workflows: nightly · triage — open a run in ^w and press r to re-run its script"
        );
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        // No conversation open: saved workflows live in $BOUGH_HOME, so this
        // one answers before the first turn rather than refusing.
        app.apply(Action::Run(Command::SavedShow, String::new()), 0);
        assert_eq!(sends(&effects), vec![Effect::LoadSaved]);
        app.apply(Action::Saved(vec![saved("nightly")]), 1);
        assert_eq!(
            app.notice.as_deref(),
            Some("1 saved workflow: nightly — open a run in ^w and press r to re-run its script")
        );
    }

    /// Port of store.test.ts "the artifact list names them and does not try to
    /// fit their URLs". A notice is ONE line, and one artifact's href is 111
    /// characters — the list used to be clipped mid-URL, losing the only half
    /// the reader wanted.
    #[test]
    fn the_artifact_list_names_them_and_does_not_try_to_fit_their_urls() {
        let rows = vec![
            artifact("line_counts.html", &"a".repeat(36)),
            artifact("report.html", &"b".repeat(36)),
        ];
        let notice = describe_artifacts(&rows);
        assert!(
            notice.starts_with("2 artifacts: line_counts.html, report.html"),
            "{notice}"
        );
        assert!(!notice.contains("http://"), "{notice}");
        assert!(notice.chars().count() <= 100, "{}: {notice}", notice.len());
        assert_eq!(
            describe_artifacts(&[]),
            "this conversation has published no artifacts"
        );
    }

    /// Both conversation-scoped verbs say WHY before the first turn instead of
    /// showing an empty list, which reads as a fact about this conversation.
    #[test]
    fn artifacts_and_rules_refuse_with_a_reason_when_no_conversation_is_open() {
        for (command, sentence) in [
            (Command::ArtifactsShow, NO_CONVERSATION_ARTIFACTS),
            (Command::RulesShow, NO_CONVERSATION_RULES),
        ] {
            let (effects, sink) = scripted();
            let mut app = App::new(TuiOptions::default(), sink, 100, 24);
            app.apply(Action::Run(command, String::new()), 0);
            assert_eq!(app.notice.as_deref(), Some(sentence));
            assert!(sends(&effects).is_empty(), "nothing to ask the server for");
        }
    }

    #[test]
    fn artifacts_and_rules_ask_for_their_own_listing_once_a_conversation_is_open() {
        for (command, effect, action, expected) in [
            (
                Command::ArtifactsShow,
                Effect::LoadArtifacts,
                Action::Artifacts(vec![artifact("report.html", "s1")]),
                "1 artifact: report.html — the link is on the turn that published each one",
            ),
            (
                Command::RulesShow,
                Effect::LoadProjectRules,
                Action::ProjectRules(vec![rule("AGENTS.md", 120)]),
                "1 AGENTS.md in every turn's prompt, in this order: AGENTS.md (120 chars)",
            ),
        ] {
            let (effects, sink) = scripted();
            let mut app = App::new(TuiOptions::default(), sink, 100, 24);
            open_s1(&mut app);
            app.apply(Action::Run(command, String::new()), 0);
            assert_eq!(sends(&effects), vec![effect]);
            app.apply(action, 1);
            assert_eq!(app.notice.as_deref(), Some(expected));
        }
    }

    /// The rules line is ORDERED, and says so: two files are a precedence
    /// question, and the answer is the one that ships in the prompt last.
    #[test]
    fn the_rules_line_is_in_prompt_order_and_names_the_winner() {
        assert_eq!(
            describe_project_rules(&[]),
            "no AGENTS.md applies here — write one in the workspace, \
             or in $BOUGH_HOME for every project"
        );
        assert_eq!(
            describe_project_rules(&[rule("AGENTS.md", 120), rule("packages/api/AGENTS.md", 40)]),
            "2 AGENTS.mds in every turn's prompt, in this order: AGENTS.md (120 chars) → \
             packages/api/AGENTS.md (40 chars) — the last one wins where two disagree"
        );
    }

    /// Port of App.test.tsx "^g copies the OPEN conversation's id, and says
    /// so" — and its no-conversation twin. The id is the handle every
    /// out-of-band route back to this run needs.
    #[test]
    fn ctrl_g_copies_the_open_conversations_id_and_says_so() {
        let copied: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        {
            let sink = copied.clone();
            app.set_copy(Box::new(move |t| sink.lock().unwrap().push(t.to_string())));
        }
        // With nothing open there is no id to copy, and it says why.
        app.apply(ctrl('g'), 0);
        assert!(copied.lock().unwrap().is_empty(), "there is no id to copy");
        assert_eq!(app.notice.as_deref(), Some(NO_CONVERSATION_TO_COPY));

        open_s1(&mut app);
        app.apply(ctrl('g'), 1);
        assert_eq!(copied.lock().unwrap().as_slice(), ["s1".to_string()]);
        assert_eq!(app.notice.as_deref(), Some("copied s1"));
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

    // ---- the status bar and the memory margin, through the COMPOSITION ROOT --
    //
    // Three facts existed at both ends and in the middle not at all: the meter
    // has carried a model, a cost and a context chip since it was written, the
    // transcript has carried both `#` rows since IT was written, and `app.rs`
    // passed `ChatMeter { workspace, help }` and two empty vectors. Every
    // assertion below reads the REAL frame, so a renderer nobody feeds fails
    // here with its own module's tests green.

    fn meta(session: bough_core::schema::parts::Session) -> Action {
        Action::SessionMeta(Box::new(SessionMeta {
            session,
            usage: crate::api::SnapshotUsage {
                totals: bough_core::types::UsageTotals::default(),
                tree: bough_core::types::UsageTotals::default(),
            },
            effective_model: Some("openai/gpt-5.6-luna".into()),
            context_limit: Some(200_000),
            primed_tags: vec!["git:push".into()],
            project_rules: vec![crate::api::ProjectRuleSummary {
                label: "AGENTS.md".into(),
                path: "/w/AGENTS.md".into(),
                bytes: 120,
            }],
        }))
    }

    /// A session row as `GET /sessions/:id` reports it.
    fn snap_session(id: &str) -> bough_core::schema::parts::Session {
        let mut s = crate::forest::fixtures::session_row(
            id,
            bough_core::schema::parts::SessionKind::Root,
            0,
        )
        .session;
        s.workspace = Some("/repos/bough".into());
        s.context_tokens = Some(50_000);
        s
    }

    #[test]
    fn opening_a_conversation_asks_for_everything_the_status_bar_is_made_of() {
        let (effects, sink) = scripted();
        let mut app = App::new(
            TuiOptions {
                workspace: Some("/repos/bough".into()),
            },
            sink,
            100,
            24,
        );
        open_s1(&mut app);
        // The snapshot half: model, context window, tags, rules.
        assert!(effects.borrow().contains(&Effect::LoadSessionMeta));
        // …and the branch and the defaults, on the first tick.
        app.apply(Action::Tick, 0);
        assert!(effects
            .borrow()
            .contains(&Effect::LoadBranch("/repos/bough".into())));
        assert!(effects.borrow().contains(&Effect::LoadModelSettings));
    }

    #[test]
    fn the_status_bar_names_the_model_the_branch_and_what_is_left_of_the_context() {
        let (_effects, sink) = scripted();
        let mut app = App::new(
            TuiOptions {
                workspace: Some("/repos/bough".into()),
            },
            sink,
            100,
            24,
        );
        open_s1(&mut app);
        app.apply(meta(snap_session("s1")), 0);
        app.apply(
            Action::Branch {
                dir: "/repos/bough".into(),
                branch: "main".into(),
            },
            0,
        );
        let frame = frame_of(&app, 100, 24);
        let bar = frame.lines().last().unwrap().trim_end().to_string();
        assert_eq!(
            bar, "/repos/bough@main · openai/gpt-5.6-luna · 75% ctx left · ? help",
            "the status bar"
        );
    }

    #[test]
    fn the_meter_stays_quiet_about_what_it_has_not_been_told() {
        let (_effects, sink) = scripted();
        let app = App::new(
            TuiOptions {
                workspace: Some("/repos/bough".into()),
            },
            sink,
            100,
            24,
        );
        // No snapshot, no branch, no defaults: the bar degrades to silence
        // rather than to a fake model or a 0% chip.
        let frame = frame_of(&app, 100, 24);
        let bar = frame.lines().last().unwrap().trim_end().to_string();
        assert_eq!(bar, "/repos/bough · ? help");
    }

    #[test]
    fn the_cost_and_the_context_are_live_rather_than_read_once_at_open() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        app.apply(meta(snap_session("s1")), 0);
        // A turn is running: the two feeds that move ride the poll tick.
        app.apply(
            event(
                EventType::MessageStarted,
                1,
                json!({
                    "id": "m1", "sessionId": "s1", "role": "user", "pending": true,
                    "parts": [{"type": "text", "text": "go"}], "createdAt": 1
                }),
            ),
            1,
        );
        effects.borrow_mut().clear();
        for t in 0..POLL_TICKS {
            app.apply(Action::Tick, 2 + t as i64);
        }
        assert!(effects.borrow().contains(&Effect::PollUsage), "spend");
        assert!(
            effects.borrow().contains(&Effect::LoadSessionMeta),
            "context"
        );
        // The poll's answer moves the number on screen without a snapshot.
        app.apply(
            Action::Usage(crate::api::SnapshotUsage {
                totals: bough_core::types::UsageTotals::default(),
                tree: bough_core::types::UsageTotals {
                    cost_usd: 0.042,
                    ..Default::default()
                },
            }),
            3,
        );
        assert!(frame_of(&app, 100, 24).contains("$0.042"));
        // …and the settle re-reads both: the round that just ended is the one
        // that moved them.
        effects.borrow_mut().clear();
        app.apply(
            event(
                EventType::TurnFinished,
                4,
                json!({"turnId": "t1", "sessionId": "s1", "status": "done"}),
            ),
            4,
        );
        assert!(effects.borrow().contains(&Effect::LoadSessionMeta));
        assert!(effects.borrow().contains(&Effect::PollUsage));
    }

    #[test]
    fn the_two_hash_rows_open_the_transcript_tags_above_rules() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        app.apply(meta(snap_session("s1")), 0);
        app.apply(
            Action::Thread(vec![crate::forest::fixtures::msg(
                "m1",
                Role::User,
                "hello",
            )]),
            0,
        );
        let lines = app.transcript_lines();
        assert_eq!(lines[0], "# this repo remembers: git:push");
        assert_eq!(lines[1], "# rules: AGENTS.md · /rules");
        // …and they are on the SCREEN, not merely in the vector.
        let frame = frame_of(&app, 100, 24);
        assert!(frame.contains("# rules: AGENTS.md · /rules"), "{frame}");
    }

    #[test]
    fn a_repo_with_no_agents_md_and_no_tags_gets_no_margin_rows() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        app.apply(
            Action::SessionMeta(Box::new(SessionMeta {
                session: snap_session("s1"),
                usage: crate::api::SnapshotUsage {
                    totals: bough_core::types::UsageTotals::default(),
                    tree: bough_core::types::UsageTotals::default(),
                },
                effective_model: None,
                context_limit: None,
                primed_tags: Vec::new(),
                project_rules: Vec::new(),
            })),
            0,
        );
        assert!(
            app.transcript_lines().is_empty(),
            "no row, not an empty one"
        );
    }

    #[test]
    fn another_conversations_snapshot_never_relabels_this_screen() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        app.apply(meta(snap_session("s2")), 0);
        assert!(app.primed_tags.is_empty(), "a snapshot that lost the race");
        assert!(app.meter().model.is_none());
        // …and a branch reply for a checkout this screen has left.
        app.apply(
            Action::Branch {
                dir: "/somewhere/else".into(),
                branch: "wip".into(),
            },
            0,
        );
        assert!(app.branch.is_none());
    }

    #[test]
    fn a_session_switch_drops_the_meter_and_the_margin_rows_it_was_showing() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        app.apply(meta(snap_session("s1")), 0);
        assert!(!app.project_rules.is_empty());
        app.apply(Action::SessionOpened("s2".into()), 1);
        assert!(app.project_rules.is_empty(), "another session's rule sheet");
        assert!(app.primed_tags.is_empty());
        assert!(app.meter().context_tokens.is_none());
        assert!(app.meter().cost_usd.is_none());
    }

    #[test]
    fn the_live_counts_on_the_bar_are_the_rails_own_rows() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        app.apply(meta(snap_session("s1")), 0);
        app.apply(Action::Jobs(vec![job("j1", "sleep 5")]), 0);
        let meter = app.meter();
        assert_eq!(meter.shells, Some(1));
        assert!(frame_of(&app, 120, 24).contains("⚙ 1 shell"));
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
        assert_eq!(sends(&effects), vec![a_send("add a test".into())]);
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
            vec![a_send("add a test".into()), Effect::Interrupt]
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

    /// A running job as the listing carries it: the row, not the bare job.
    fn job(id: &str, command: &str) -> crate::api::JobListRow {
        crate::api::JobListRow {
            job: bare_job(id, command),
            tail: None,
            output_lines: None,
        }
    }

    fn bare_job(id: &str, command: &str) -> BackgroundJob {
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
                job: Some(bare_job("job-1", "sleep 30")),
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
            &[a_send("/model is the wrong word here".into())]
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
        assert_eq!(sends(&effects).as_slice(), &[a_send("@zzzz".into())]);
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

    /// A frame's cells at one row, as (symbol, bg).
    fn cells_at(app: &App<impl FnMut(Effect)>, row: u16) -> Vec<(String, Style)> {
        let area = Rect {
            x: 0,
            y: 0,
            width: app.cols.max(20),
            height: app.rows.max(8),
        };
        let mut buf = Buffer::empty(area);
        app.draw(area, &mut buf);
        (0..area.width)
            .map(|x| {
                let c = &buf[(x, row)];
                (c.symbol().to_string(), c.style())
            })
            .collect()
    }

    /// The bug this pins: `self.sel` was tracked by the mouse handler, the copy
    /// worked, and NO render path ever read it — so a drag highlighted nothing
    /// at all and the cells under it reported the default background the whole
    /// time. The reducer being right is not the feature; being visible is.
    #[test]
    fn a_drag_paints_the_cells_it_covers_and_clears_them_on_release() {
        let (mut app, _copied, _opened) = app_with_capture();
        open_s1(&mut app);
        type_text(&mut app, "hello selection", 0);
        app.apply(key(KeyCode::Enter), 10);

        // A row with something painted on it — find one rather than assume.
        let row = (0..app.rows)
            .find(|y| {
                cells_at(&app, *y)
                    .iter()
                    .take(12)
                    .any(|(s, _)| s.trim() != "")
            })
            .expect("some row has text on it");
        let before = cells_at(&app, row);
        let accent = crate::theme::palette().accent_color();
        assert!(
            before.iter().all(|(_, st)| st.bg != Some(accent)),
            "nothing is highlighted before the drag"
        );

        // Press and drag across the first ten cells. Mouse reports are 0-based;
        // a selection is 1-based, so screen row `row` is selection row `row+1`.
        app.apply(mouse(MouseEventKind::Down(MouseButton::Left), 0, row), 20);
        app.apply(mouse(MouseEventKind::Drag(MouseButton::Left), 9, row), 21);

        let during = cells_at(&app, row);
        let painted: Vec<usize> = (0..10)
            .filter(|x| during[*x].1.bg == Some(accent))
            .collect();
        assert_eq!(
            painted,
            (0..10).collect::<Vec<_>>(),
            "every dragged cell is visually distinct mid-drag"
        );
        // …and the highlight stops where the drag stopped.
        assert_ne!(
            during[10].1.bg,
            Some(accent),
            "the cell past the drag is untouched"
        );
        // The TEXT is unchanged — a highlight recolours, it does not overwrite.
        let syms: Vec<&String> = during.iter().map(|(s, _)| s).collect();
        assert_eq!(syms, before.iter().map(|(s, _)| s).collect::<Vec<_>>());

        app.apply(mouse(MouseEventKind::Up(MouseButton::Left), 9, row), 22);
        let after = cells_at(&app, row);
        assert!(
            after.iter().all(|(_, st)| st.bg != Some(accent)),
            "the highlight is gone once the selection is dropped"
        );
    }

    /// A drag over blank cells must not hang a bar of accent off the end of a
    /// short line (App.tsx: `if (text.trim())`).
    #[test]
    fn a_drag_over_nothing_paints_nothing() {
        let (mut app, _copied, _opened) = app_with_capture();
        open_s1(&mut app);
        // A row that is entirely blank in the padded region above the transcript.
        let row = (0..app.rows)
            .find(|y| cells_at(&app, *y).iter().all(|(s, _)| s.trim() == ""))
            .expect("some row is blank");
        app.apply(mouse(MouseEventKind::Down(MouseButton::Left), 0, row), 0);
        app.apply(mouse(MouseEventKind::Drag(MouseButton::Left), 15, row), 1);
        let accent = crate::theme::palette().accent_color();
        assert!(
            cells_at(&app, row)
                .iter()
                .all(|(_, st)| st.bg != Some(accent)),
            "an empty run is not painted"
        );
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

    // ---- the folds, and the transcript's own click targets -----------------

    /// A message with one shell step and its output — what `run: echo hello`
    /// puts in the thread.
    fn tool_msg() -> Message {
        Message {
            id: "m-tool".into(),
            session_id: "s1".into(),
            role: Role::Supervisor,
            parts: vec![
                Part::ToolCall {
                    id: "c1".into(),
                    name: "run_steps".into(),
                    input: json!({ "code": "await bash('echo hello')" }),
                },
                Part::ToolResult {
                    call_id: "c1".into(),
                    output: json!("hello"),
                    is_error: false,
                    interrupted: None,
                },
            ],
            pending: false,
            created_at: 1,
        }
    }

    fn transcript(app: &App<impl FnMut(Effect)>) -> String {
        app.transcript_lines().join("\n")
    }

    #[test]
    fn ctrl_e_unfolds_every_tool_call_and_folds_them_back() {
        let (mut app, _c, _o) = app_with_capture();
        open_s1(&mut app);
        app.thread.push(tool_msg());
        // Folded is the resting state: the header, and nothing it did.
        assert!(
            transcript(&app).contains("▸ 1 step"),
            "{}",
            transcript(&app)
        );
        // Collapsed still says WHAT it did — the gist rides the header — but
        // not what it printed.
        assert!(
            transcript(&app).contains("▸ 1 step · await bash('echo hello')"),
            "{}",
            transcript(&app)
        );
        assert!(!transcript(&app).contains("hello\n"));

        app.apply(ctrl('e'), 0);
        let open = transcript(&app);
        assert!(open.contains("▾ 1 step"), "{open}");
        assert!(open.contains("await bash('echo hello')"), "{open}");
        assert!(open.contains("↳ output"), "{open}");
        assert!(open.contains("hello"), "{open}");

        app.apply(ctrl('e'), 1);
        assert!(transcript(&app).contains("▸ 1 step"));
        assert!(!transcript(&app).contains("↳ output"));
    }

    #[test]
    fn ctrl_e_drops_the_per_group_state_so_the_global_toggle_wins() {
        let (mut app, _c, _o) = app_with_capture();
        open_s1(&mut app);
        app.thread.push(tool_msg());
        app.click_target("m-tool:0");
        app.click_target("m-tool:0!full");
        assert!(app.is_expanded("m-tool:0"));
        // ^e once: everything opens, and the per-group state is gone.
        app.apply(ctrl('e'), 0);
        assert!(app.fold_all);
        assert!(app.open_keys.is_empty() && app.full_keys.is_empty());
        // ^e twice is a RESET, not a return to whatever was open before.
        app.apply(ctrl('e'), 1);
        assert!(!app.fold_all);
        assert!(
            !app.is_expanded("m-tool:0"),
            "the group it had open is closed too"
        );
    }

    #[test]
    fn ctrl_e_with_a_draft_is_still_end_of_line() {
        // The binding is guarded on an empty draft; with text typed, ^e must
        // stay the composer's "end of line" and fold nothing.
        let (mut app, _c, _o) = app_with_capture();
        open_s1(&mut app);
        app.thread.push(tool_msg());
        type_text(&mut app, "hi", 0);
        app.cursor = 0;
        app.apply(ctrl('e'), 1);
        assert!(!app.fold_all);
        assert_eq!(app.cursor, 2);
    }

    /// The 1-based screen row a string is painted on.
    fn row_of(app: &App<impl FnMut(Effect)>, needle: &str) -> u16 {
        let painted = app.painted_rows();
        painted
            .iter()
            .position(|r| r.contains(needle))
            .map(|i| i as u16)
            .unwrap_or_else(|| panic!("{needle:?} is not on screen:\n{}", painted.join("\n")))
    }

    fn click_row(app: &mut App<impl FnMut(Effect)>, row: u16, now: i64) {
        app.apply(mouse(MouseEventKind::Down(MouseButton::Left), 4, row), now);
        app.apply(
            mouse(MouseEventKind::Up(MouseButton::Left), 4, row),
            now + 1,
        );
    }

    #[test]
    fn clicking_a_tool_group_toggles_that_group_and_any_of_its_rows_folds_it() {
        let (mut app, _c, _o) = app_with_capture();
        open_s1(&mut app);
        app.thread.push(tool_msg());
        let head = row_of(&app, "▸ 1 step");
        click_row(&mut app, head, 0);
        assert!(
            app.is_expanded("m-tool:0"),
            "the header row opened its group"
        );
        assert!(transcript(&app).contains("↳ output"));
        assert!(!app.fold_all, "one group, not all of them");

        // EVERY row of the fold carries the same key, so clicking the output
        // row — not the header — collapses it again.
        let out = row_of(&app, "↳ output");
        click_row(&mut app, out, 2);
        assert!(
            !app.is_expanded("m-tool:0"),
            "any row of the fold collapses it"
        );
    }

    #[test]
    fn the_more_lines_row_lifts_the_cap_and_does_not_fold_the_group() {
        let (mut app, _c, _o) = app_with_capture();
        open_s1(&mut app);
        let long: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
        let mut m = tool_msg();
        m.parts[1] = Part::ToolResult {
            call_id: "c1".into(),
            output: json!(long.join("\n")),
            is_error: false,
            interrupted: None,
        };
        app.thread.push(m);
        app.click_target("m-tool:0");
        let capped = transcript(&app);
        assert!(capped.contains("… +20 more lines"), "{capped}");
        assert!(!capped.contains("line 39"), "{capped}");
        app.click_target("m-tool:0!full");
        let lifted = transcript(&app);
        assert!(lifted.contains("line 39"), "{lifted}");
        assert!(!lifted.contains("more lines"), "{lifted}");
        assert!(app.is_expanded("m-tool:0"), "lifting a cap is not folding");
    }

    #[test]
    fn a_thinking_block_folds_on_the_same_gesture() {
        let (mut app, _c, _o) = app_with_capture();
        open_s1(&mut app);
        app.thread.push(Message {
            id: "m-think".into(),
            session_id: "s1".into(),
            role: Role::Supervisor,
            parts: vec![Part::Reasoning {
                text: "first thought\nsecond thought".into(),
                meta: None,
                model: None,
            }],
            pending: false,
            created_at: 1,
        });
        assert!(transcript(&app).contains("▸ thinking · first thought"));
        app.click_target("m-think:0");
        let open = transcript(&app);
        assert!(open.contains("▾ thinking (2 lines)"), "{open}");
        assert!(open.contains("second thought"), "{open}");
    }

    /// THE GAP THIS CLOSED. `build_lines` emits SGR into every row; painting
    /// those rows raw prints `[0m` on screen. Nothing the transcript draws may
    /// contain an escape.
    #[test]
    fn the_painted_transcript_carries_no_escape_sequences() {
        let (mut app, _c, _o) = app_with_capture();
        open_s1(&mut app);
        app.primed_tags = vec!["git:push".into()];
        app.project_rules = vec!["AGENTS.md".into()];
        app.thread.push(tool_msg());
        app.thread.push(Message {
            id: "m-md".into(),
            session_id: "s1".into(),
            role: Role::Supervisor,
            parts: vec![Part::Text {
                text: "**bold** and `code` and a [link](https://bough.dev)".into(),
            }],
            pending: false,
            created_at: 2,
        });
        let screen = app.painted_rows().join("\n");
        assert!(!screen.contains('\u{1b}'), "{screen}");
        assert!(!screen.contains("[0m"), "{screen}");
        assert!(!screen.contains("[2m"), "{screen}");
        // …and the markdown is RENDERED, not printed as source.
        assert!(screen.contains("bold"), "{screen}");
        assert!(!screen.contains("**bold**"), "{screen}");
    }

    /// An exited shell leaves a CARD in the transcript, and the card's own row
    /// opens that job's output. Before the real builder was wired the card was
    /// never emitted, so `job:` was a target nothing on screen could produce.
    #[test]
    fn an_exited_job_draws_a_card_whose_row_opens_it() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        let mut row = job("j7", "sleep 25");
        row.job.status = bough_core::schema::parts::JobStatus::Exited;
        row.job.exit_code = Some(0);
        row.job.exited_at = Some(5_000);
        row.tail = Some(vec!["done sleeping".into()]);
        row.output_lines = Some(1);
        app.apply(Action::Jobs(vec![row]), 6_000);

        let lines = app.transcript_vlines();
        let card = lines
            .iter()
            .find(|l| l.click.as_deref().is_some_and(|c| c.starts_with("job:")))
            .unwrap_or_else(|| panic!("no job card: {:?}", app.transcript_lines()));
        assert_eq!(card.click.as_deref(), Some("job:s1:j7"));
        assert!(
            app.transcript_lines()
                .iter()
                .any(|l| l.contains("sleep 25")),
            "{:?}",
            app.transcript_lines()
        );
        // And the click that row resolves to really opens the output.
        let target = card.click.clone().unwrap();
        app.click_target(&target);
        assert_eq!(app.job.as_ref().map(|v| v.id.as_str()), Some("j7"));
        assert!(sends(&effects).contains(&Effect::LoadJobOutput("j7".into())));
    }

    /// A finished subagent's report becomes a BRANCH CARD anchored under the
    /// message that spawned it, and the card descends into that branch. The
    /// feed is the two halves this client already had: the delegated children
    /// polled for the rail, and the completion note in the thread.
    #[test]
    fn a_finished_subagents_report_becomes_a_card_that_descends() {
        use bough_core::schema::parts::SessionKind;
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.thread.push(Message {
            id: "m1".into(),
            session_id: "s1".into(),
            role: Role::User,
            parts: vec![Part::Text {
                text: "delegate the audit".into(),
            }],
            pending: false,
            created_at: 1,
        });
        app.thread.push(Message {
            id: "m2".into(),
            session_id: "s1".into(),
            role: Role::System,
            parts: vec![Part::Text {
                text: "[subagent finished] \"audit app.rs\" (sub-1) — finished.\n\
                       Changed files: app.rs.\n\
                       Report:\nthe hit-test was fine\n\
                       It worked in THIS session's checkout"
                    .into(),
            }],
            pending: false,
            created_at: 2,
        });
        let mut child = crate::forest::fixtures::session_row("sub-1", SessionKind::Subagent, 2);
        child.session.title = "audit app.rs".into();
        child.session.parent_id = Some("s1".into());
        child.session.origin_message_id = Some("m1".into());
        child.session.outcome_ok = Some(true);
        app.apply(Action::Sessions(vec![child]), 3);

        let lines = app.transcript_vlines();
        let card = lines
            .iter()
            .find(|l| l.click.as_deref() == Some("open:sub-1"))
            .unwrap_or_else(|| panic!("no branch card: {:?}", app.transcript_lines()));
        assert!(card.click.is_some());
        let plain = app.transcript_lines().join("\n");
        assert!(plain.contains("audit app.rs"), "{plain}");
        assert!(plain.contains("the hit-test was fine"), "{plain}");
        // The RAW note is gone: never both, never neither.
        assert!(!plain.contains("[subagent finished]"), "{plain}");
        app.click_target("open:sub-1");
        assert!(sends(&effects).contains(&Effect::OpenSession("sub-1".into())));
    }

    /// A running program's console lines show WHILE it runs, and the finalized
    /// result REPLACES them rather than printing them twice.
    #[test]
    fn tool_log_lines_stream_under_a_running_call_and_are_replaced_by_the_result() {
        let (mut app, _c, _o) = app_with_capture();
        open_s1(&mut app);
        app.thread.push(Message {
            id: "m-tool".into(),
            session_id: "s1".into(),
            role: Role::Supervisor,
            parts: vec![Part::ToolCall {
                id: "c1".into(),
                name: "run_steps".into(),
                input: json!({ "code": "await bash('echo hello')" }),
            }],
            pending: false,
            created_at: 1,
        });
        app.apply(ctrl('e'), 0); // open every fold
        app.apply(
            Action::Event(BoughEvent {
                r#type: EventType::ToolLog,
                seq: 1,
                ts: 1,
                session_id: Some("s1".into()),
                data: json!({"messageId": "m-tool", "callId": "c1", "line": "hello"}),
            }),
            1,
        );
        let live = transcript(&app);
        assert!(live.contains("↳ output (live)"), "{live}");
        assert_eq!(
            live.matches("hello").count(),
            2,
            "the gist and the log: {live}"
        );

        // The result lands: the live block goes, the finalized one takes over.
        app.thread[0].parts.push(Part::ToolResult {
            call_id: "c1".into(),
            output: json!("hello"),
            is_error: false,
            interrupted: None,
        });
        let done = transcript(&app);
        assert!(!done.contains("(live)"), "{done}");
        assert!(done.contains("↳ output"), "{done}");
    }

    /// A settled turn leaves a mark in the transcript's ledger — the numbers
    /// the spinner was showing do not simply vanish when it stops.
    #[test]
    fn a_finished_turn_leaves_a_settled_mark_in_the_transcript() {
        let (mut app, _c, _o) = app_with_capture();
        open_s1(&mut app);
        app.apply(
            Action::Event(BoughEvent {
                r#type: EventType::MessageStarted,
                seq: 1,
                ts: 1_000,
                session_id: Some("s1".into()),
                data: serde_json::to_value(Message {
                    id: "m1".into(),
                    session_id: "s1".into(),
                    role: Role::Supervisor,
                    parts: vec![],
                    pending: true,
                    created_at: 1_000,
                })
                .unwrap(),
            }),
            1_000,
        );
        app.apply(
            Action::Event(BoughEvent {
                r#type: EventType::TurnFinished,
                seq: 2,
                ts: 15_000,
                session_id: Some("s1".into()),
                data: json!({"turnId": "t1", "sessionId": "s1", "status": "done"}),
            }),
            15_000,
        );
        let plain = transcript(&app);
        assert!(plain.contains("✓ 14s"), "{plain}");
    }

    #[test]
    fn a_branch_card_click_descends_and_a_job_card_click_opens_that_job() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.click_target("open:s2");
        assert!(sends(&effects).contains(&Effect::OpenSession("s2".into())));
        app.click_target("job:s1:j7");
        assert_eq!(app.job.as_ref().map(|v| v.id.as_str()), Some("j7"));
        assert!(sends(&effects).contains(&Effect::LoadJobOutput("j7".into())));
    }

    #[test]
    fn a_click_where_the_panel_displaces_the_transcript_folds_nothing() {
        let (mut app, _c, _o) = app_with_capture();
        open_s1(&mut app);
        app.thread.push(tool_msg());
        let head = row_of(&app, "▸ 1 step");
        app.apply(ctrl('t'), 0);
        assert!(app.panel.open());
        click_row(&mut app, head, 1);
        assert!(app.open_keys.is_empty(), "the panel owns that region");
    }

    // ---- the binding table is a PROMISE ------------------------------------

    /// Commands with NO `Command::` arm anywhere, because a raw `KeyCode` arm
    /// in `on_key`/`on_completion_key` answers their keys by shape instead.
    ///
    /// WHAT THIS LIST IS FOR, and the one thing it must never become: it is an
    /// escape hatch for keys the composer answers WITHOUT naming the command,
    /// and it is only ever legitimate when there is a real raw arm behind the
    /// entry. It shipped as a way to silence exactly the bug it was supposed to
    /// catch — "CursorWordLeft"/"CursorWordRight"/"CursorUp"/"CursorDown" and
    /// the kills sat here for months while `on_key`'s raw match had no arm for
    /// ⌥b, ⌥f, ^w or ↑/↓ at all, so ⌥b moved nothing and ^w deleted nothing.
    /// Listing a command here is a CLAIM that the composer answers it; the
    /// entries are now routed through `keys::edit_line` and name their commands,
    /// so they are gone from this list rather than trusted in it.
    ///
    /// `allowlisted_commands_are_not_secretly_handled` below is the guard: an
    /// entry that DOES have a `Command::` arm is stale and must be deleted, so
    /// the list can only ever shrink as commands get properly routed.
    const HANDLED_BY_THE_COMPOSER: &[&str] = &[
        "SendQueue",
        "Newline",
        "DraftClear",
        "AttachmentUp",
        "AttachmentDown",
        "CompleteAccept",
        "CompletePrev",
        "CompleteNext",
        "CompleteDismiss",
    ];

    /// Every `.rs` in this crate except the binding table itself — the table is
    /// where commands are DECLARED, so finding a name there proves nothing
    /// about anything handling it.
    fn crate_sources() -> String {
        let mut sources = String::new();
        let mut stack = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs")
                    && path.file_name().is_some_and(|f| f != "keys.rs")
                {
                    sources.push_str(&std::fs::read_to_string(&path).unwrap());
                }
            }
        }
        sources
    }

    /// The teeth. An allowlist entry is a claim that the composer answers a
    /// chord some other way; if the command HAS a `Command::` arm the claim is
    /// stale and the entry is now hiding whatever that arm does or fails to do.
    #[test]
    fn allowlisted_commands_are_not_secretly_handled() {
        let sources = crate_sources();
        let stale: Vec<&str> = HANDLED_BY_THE_COMPOSER
            .iter()
            .copied()
            .filter(|name| sources.contains(&format!("Command::{name}")))
            .collect();
        assert_eq!(
            stale,
            Vec::<&str>::new(),
            "these are handled by a real `Command::` arm — delete them from \
             HANDLED_BY_THE_COMPOSER so the pin can see them"
        );
    }

    // ---- the line editor is REACHABLE ---------------------------------------
    //
    // Every one of these chords was in the binding table, printed in the help
    // overlay, and dead on the real binary: `on_key`'s raw `KeyCode` match knew
    // ^a/^e/home/end/backspace/←/→ and nothing else, so ⌥b appended at the end
    // and ^w left the draft untouched. These drive the real keymap into the
    // real `App` (ported from `keys.test.ts`'s editor cases, but through the
    // app rather than through `editLine` directly — the pure function was
    // always green, which is exactly why nobody caught this).

    fn drafted(text: &str) -> (App<impl FnMut(Effect)>, ()) {
        let (mut app, _, _) = app_with_capture();
        open_s1(&mut app);
        type_text(&mut app, text, 0);
        (app, ())
    }

    #[test]
    fn meta_b_moves_the_cursor_a_word_back_and_typing_lands_there() {
        let (mut app, _) = drafted("hello world there");
        app.apply(meta_key(KeyCode::Char('b')), 1);
        app.apply(meta_key(KeyCode::Char('b')), 2);
        assert_eq!(app.cursor, 6, "two words back from the end of the line");
        type_text(&mut app, "X", 3);
        assert_eq!(app.draft, "hello Xworld there");
    }

    #[test]
    fn meta_f_moves_the_cursor_a_word_forward() {
        let (mut app, _) = drafted("hello world there");
        app.apply(ctrl('a'), 1);
        app.apply(meta_key(KeyCode::Char('f')), 2);
        assert_eq!(app.cursor, 5);
        app.apply(meta_key(KeyCode::Right), 3);
        assert_eq!(app.cursor, 11, "⌥→ is the same command as ⌥f");
    }

    #[test]
    fn ctrl_w_deletes_the_word_before_the_cursor() {
        let (mut app, _) = drafted("hello world there");
        app.apply(ctrl('w'), 1);
        assert_eq!(app.draft, "hello world ");
        assert_eq!(app.cursor, 12);
        // ⌥⌫ is the same command, and it is bound with no empty-draft guard.
        app.apply(meta_key(KeyCode::Backspace), 2);
        assert_eq!(app.draft, "hello ");
    }

    #[test]
    fn ctrl_k_and_ctrl_u_kill_to_the_end_and_the_whole_line() {
        let (mut app, _) = drafted("hello world there");
        app.apply(meta_key(KeyCode::Char('b')), 1);
        app.apply(ctrl('k'), 2);
        assert_eq!(app.draft, "hello world ");
        app.apply(ctrl('u'), 3);
        assert_eq!(app.draft, "");
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn up_and_down_walk_the_lines_of_a_multiline_draft() {
        let (mut app, _) = drafted("hello");
        app.apply(ctrl('j'), 1); // ^j inserts the newline
        type_text(&mut app, "world", 2);
        assert_eq!(app.draft, "hello\nworld");
        assert_eq!(app.cursor, 11);

        app.apply(key(KeyCode::Up), 3);
        assert_eq!(app.cursor, 5, "column 5 of the first line");
        app.apply(key(KeyCode::Down), 4);
        assert_eq!(app.cursor, 11, "and back down");

        // Typing after ↑ lands on the FIRST line, which is the whole point.
        app.apply(key(KeyCode::Up), 5);
        type_text(&mut app, "!", 6);
        assert_eq!(app.draft, "hello!\nworld");
    }

    #[test]
    fn up_is_not_a_line_move_when_the_draft_is_a_single_line() {
        // The `multiline` guard: with one line ↑ belongs to the history ring,
        // not the editor, and must not silently eat the key as a no-op edit.
        let (mut app, _) = drafted("hello");
        app.apply(key(KeyCode::Up), 1);
        assert_eq!(app.draft, "hello");
        assert_eq!(app.cursor, 5, "cursor did not move");
    }

    #[test]
    fn every_bound_chord_is_actually_handled_not_merely_listed() {
        // `dead_bindings` proves the TABLE is self-consistent. It cannot see
        // the other way a row lies: `^e` was in the table, documented in the
        // help overlay as "fold/unfold every tool call", and no arm anywhere
        // read `Command::FoldAll` — so the overlay promised a gesture the app
        // ignored, forever. A binding the help promises and the app drops is a
        // lie; this is the pin.
        let sources = crate_sources();
        let mut unhandled: Vec<String> = Vec::new();
        for binding in crate::keys::BINDINGS.iter() {
            // `Tab(_)` is one arm for seven rows; name the variant, not the payload.
            let name = format!("{:?}", binding.command);
            let name = name.split('(').next().unwrap().to_string();
            if HANDLED_BY_THE_COMPOSER.contains(&name.as_str()) {
                continue;
            }
            if !sources.contains(&format!("Command::{name}")) {
                unhandled.push(format!("{} → {name}", binding.chord));
            }
        }
        unhandled.sort();
        unhandled.dedup();
        assert_eq!(
            unhandled,
            Vec::<String>::new(),
            "bound, documented, and dropped on the floor"
        );
    }

    // ---- the five defects a green suite let through -------------------------
    //
    // Every one of these passed a test run and failed on a real terminal, so
    // each assertion below drives the same surface a hand does: the paste event
    // the terminal actually sends, the chord the `?` overlay actually promises,
    // the Escape a user actually presses one second after Enter.

    fn paste(text: &str) -> Action {
        Action::Term(TermEvent::Paste(text.to_string()))
    }

    /// Port of `paste.test.ts` + `App.test.tsx`'s paste rows, driven through the
    /// event a bracketed paste really arrives as.
    #[test]
    fn a_paste_lands_at_the_cursor_and_is_not_swallowed() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        type_text(&mut app, "look at  please", 0);
        for _ in 0..7 {
            app.apply(key(KeyCode::Left), 0);
        }
        app.apply(paste("this bit"), 0);
        assert_eq!(app.draft, "look at this bit please");
        // …and the cursor came with it, or the next character typed lands
        // inside the text that was just pasted.
        assert_eq!(app.cursor, "look at this bit".chars().count());
    }

    #[test]
    fn a_long_paste_is_held_aside_and_its_mark_says_where_it_goes() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        let stack = "boom\n".repeat(40);
        type_text(&mut app, "explain  to me", 0);
        for _ in 0.." to me".chars().count() {
            app.apply(key(KeyCode::Left), 0);
        }
        app.apply(paste(&stack), 0);
        // The composer is not buried: one compact mark, where the cursor was.
        assert_eq!(app.draft, "explain [Pasted text #1] to me");
        assert!(app.draft.chars().count() < QUEUE_ABOVE_CHARS + 20);
        // …and the message that is SENT has the paste back, in that position.
        app.apply(key(KeyCode::Enter), 0);
        let sent = sends(&effects);
        // `strip_ctl` keeps the newlines, so the stack trace arrives whole.
        assert_eq!(
            sent.first(),
            Some(&a_send(format!(
                "explain {} to me",
                stack.replace('\r', "")
            ))),
            "the mark expanded where it sat"
        );
    }

    #[test]
    fn a_pasted_image_path_is_a_picture_and_ordinary_text_never_touches_disk() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        app.apply(paste("/tmp/shot.png"), 0);
        assert_eq!(
            sends(&effects),
            vec![Effect::AttachPath("/tmp/shot.png".into())],
            "an absolute image path is read as bytes, not typed as prose"
        );
        assert_eq!(app.draft, "", "…and nothing was inserted");
        // Anything else is words: a relative path, a non-image, a sentence
        // that merely mentions one (`clipboard.rs`).
        app.apply(paste("look at /tmp/shot.png"), 0);
        assert_eq!(app.draft, "look at /tmp/shot.png");
        assert_eq!(sends(&effects).len(), 1, "no second disk read");
    }

    #[test]
    fn a_paste_into_a_surface_that_owns_the_keyboard_is_not_typed_into_a_hidden_composer() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        app.apply(ctrl('t'), 0);
        assert!(app.panel.open());
        app.apply(paste("nope"), 0);
        assert_eq!(app.draft, "");
    }

    /// Defect 2. The clamp is not the fix: a clamped cursor still points at a
    /// row the user never highlighted, and ⏎ on the `/` list RUNS it.
    #[test]
    fn narrowing_the_popup_resets_the_highlight_instead_of_clamping_it() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        type_text(&mut app, "/", 0);
        assert!(app.completing(), "the `/` list is up");
        // Walk down to a row well inside the list.
        for _ in 0..4 {
            app.apply(key(KeyCode::Down), 0);
        }
        assert_eq!(app.completion_sel, 4);
        // Now narrow it until far fewer rows remain.
        type_text(&mut app, "the", 0);
        assert_eq!(
            app.completion_sel, 0,
            "a new candidate set starts at its own first row"
        );
        let top = app.completion().items[0].label.clone();
        app.apply(key(KeyCode::Enter), 0);
        // Whatever ran, it is the row that WAS highlighted — the only row a
        // reset can leave selected.
        let ran = sends(&effects);
        assert!(
            !ran.is_empty(),
            "the highlighted row acted: {top} · {ran:?}"
        );
        // …and a candidate list arriving late resets it too.
        type_text(&mut app, "@", 0);
        app.apply(key(KeyCode::Down), 0);
        app.apply(Action::Files(vec!["a.rs".into(), "b.rs".into()]), 0);
        assert_eq!(app.completion_sel, 0, "a fetched list is a new list");
    }

    /// Defect 3. The documented usage is Enter, then esc a second later — which
    /// is precisely the window in which the message still wears `local-N`.
    #[test]
    fn esc_right_after_enter_takes_the_message_back_without_naming_a_local_id() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        type_text(&mut app, "wait no", 0);
        app.apply(key(KeyCode::Enter), 0);
        assert!(app.notice.as_deref() == Some(TAKE_BACK_HINT));
        // One second later — inside the window, before any snapshot came back.
        app.apply(key(KeyCode::Esc), 1000);
        let acted = sends(&effects);
        assert!(
            acted.contains(&Effect::UnsendLatest {
                text: "wait no".into()
            }),
            "the take-back resolves the id server-side: {acted:?}"
        );
        // The id the server has never heard of is NEVER handed to the route.
        assert!(
            !acted
                .iter()
                .any(|e| matches!(e, Effect::Unsend(id) if id.starts_with(LOCAL_ID_PREFIX))),
            "a `local-N` id reached the unsend route: {acted:?}"
        );
    }

    #[test]
    fn a_message_the_server_has_named_is_unsent_by_that_name() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        type_text(&mut app, "wait no", 0);
        app.apply(key(KeyCode::Enter), 0);
        // The snapshot lands: the echo is now the server's row.
        app.apply(
            Action::Thread(vec![Message {
                id: "m7".into(),
                session_id: "s1".into(),
                role: Role::User,
                parts: vec![Part::Text {
                    text: "wait no".into(),
                }],
                pending: false,
                created_at: 0,
            }]),
            500,
        );
        app.apply(key(KeyCode::Esc), 1000);
        assert!(sends(&effects).contains(&Effect::Unsend("m7".into())));
    }

    /// Defect 4. The `?` overlay is generated from the keymap, so every chord it
    /// prints is a promise this client made.
    #[test]
    fn ctrl_n_starts_a_fresh_conversation_the_way_the_overlay_says() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        type_text(&mut app, "half a thought", 0);
        app.apply(key(KeyCode::Enter), 0);
        app.apply(ctrl('n'), 10);
        assert_eq!(app.session_id, None, "the old conversation was left");
        assert!(app.thread.is_empty());
        assert_eq!(app.draft, "");
        assert!(sends(&effects).contains(&Effect::NewConversation));
    }

    #[test]
    fn up_and_down_walk_what_this_screen_has_sent() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        for line in ["first", "second"] {
            type_text(&mut app, line, 0);
            app.apply(key(KeyCode::Enter), 0);
        }
        app.apply(key(KeyCode::Up), 0);
        assert_eq!(app.draft, "second");
        assert_eq!(app.cursor, 6, "the cursor is at the end, ready to edit");
        app.apply(key(KeyCode::Up), 0);
        assert_eq!(app.draft, "first");
        app.apply(key(KeyCode::Up), 0);
        assert_eq!(app.draft, "first", "the oldest line is where ↑ stops");
        app.apply(key(KeyCode::Down), 0);
        assert_eq!(app.draft, "second");
        app.apply(key(KeyCode::Down), 0);
        assert_eq!(app.draft, "", "↓ off the end returns to the empty draft");
    }

    #[test]
    fn a_shell_line_is_recalled_with_its_sigil_so_re_running_it_is_up_enter() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        type_text(&mut app, "!git status", 0);
        app.apply(key(KeyCode::Enter), 0);
        app.apply(key(KeyCode::Up), 0);
        assert_eq!(app.draft, "!git status");
        app.apply(key(KeyCode::Enter), 0);
        assert_eq!(
            sends(&effects),
            vec![
                Effect::RunShell("git status".into()),
                Effect::RunShell("git status".into())
            ]
        );
    }

    #[test]
    fn up_in_a_multiline_draft_moves_between_lines_and_never_recalls() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        type_text(&mut app, "sent already", 0);
        app.apply(key(KeyCode::Enter), 0);
        type_text(&mut app, "one", 0);
        app.apply(ctrl('j'), 0); // newline
        type_text(&mut app, "two", 0);
        assert!(app.draft.contains('\n'));
        app.apply(key(KeyCode::Up), 0);
        assert_eq!(app.draft, "one\ntwo", "the draft is untouched");
        assert!(app.cursor <= 3, "the cursor moved to the first line");
    }

    #[test]
    fn ctrl_v_asks_the_pasteboard_rather_than_doing_nothing() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        app.apply(ctrl('v'), 0);
        assert_eq!(sends(&effects), vec![Effect::ImagePaste]);
        // What comes back is an attachment, and it RIDES the message.
        app.apply(
            Action::Attached(bough_core::schema::requests::PostMessageImage {
                path: "/x/clipboard.png".into(),
                media_type: "image/png".into(),
                name: "clipboard.png".into(),
                size: 4,
            }),
            0,
        );
        assert!(
            frame_of(&app, 100, 24).contains("[image: clipboard.png]"),
            "the queued image has a row under the composer"
        );
        type_text(&mut app, "what is this", 0);
        app.apply(key(KeyCode::Enter), 0);
        match sends(&effects).last() {
            Some(Effect::Send { text, images, .. }) => {
                assert_eq!(text, "what is this");
                assert_eq!(images.len(), 1);
                assert_eq!(images[0].name, "clipboard.png");
            }
            other => panic!("{other:?}"),
        }
        assert!(
            app.attachments.is_empty(),
            "the queue emptied with the send"
        );
    }

    #[test]
    fn a_pasteboard_holding_words_is_a_paste_and_a_pasteboard_holding_nothing_says_so() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        app.apply(ctrl('v'), 0);
        app.apply(Action::PasteText("some words".into()), 0);
        assert_eq!(app.draft, "some words");
        app.apply(Action::Notice(CLIPBOARD_EMPTY.to_string()), 0);
        assert_eq!(app.notice.as_deref(), Some(CLIPBOARD_EMPTY));
    }

    #[test]
    fn the_composers_queue_does_not_follow_you_into_another_conversation() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        app.apply(
            Action::Attached(bough_core::schema::requests::PostMessageImage {
                path: "/x/a.png".into(),
                media_type: "image/png".into(),
                name: "a.png".into(),
                size: 4,
            }),
            0,
        );
        app.apply(paste(&"x".repeat(200)), 0);
        assert_eq!(app.attachments.len(), 1);
        assert_eq!(app.pastes.len(), 1);
        app.apply(ctrl('n'), 0);
        assert!(
            app.attachments.is_empty(),
            "an image queued for the thread you left"
        );
        assert!(app.pastes.is_empty());
        assert!(!frame_of(&app, 100, 24).contains("[image: a.png]"));
    }

    /// Defect 5. `!cmd` borrows the workspace's one `shell` conversation so the
    /// job has a home; it must not become the thread you are chatting in.
    #[test]
    fn a_shell_line_does_not_trap_the_next_message_in_the_shell_conversation() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        // A fresh screen: nothing open, which is where every launch starts.
        assert_eq!(app.session_id, None);
        type_text(&mut app, "!echo hi", 0);
        app.apply(key(KeyCode::Enter), 0);
        assert_eq!(sends(&effects), vec![Effect::RunShell("echo hi".into())]);
        // The transport borrows the shell conversation and says which kind it
        // is — the screen shows it so the job's output is readable.
        app.apply(Action::ShellSessionOpened("shell-1".into()), 10);
        assert_eq!(app.session_id.as_deref(), Some("shell-1"));
        // …and the first ordinary message LEAVES it rather than being typed
        // into a thread permanently titled "shell".
        type_text(&mut app, "now explain that", 20);
        app.apply(key(KeyCode::Enter), 20);
        let acted = sends(&effects);
        let new_at = acted.iter().position(|e| e == &Effect::NewConversation);
        let send_at = acted
            .iter()
            .position(|e| e == &a_send("now explain that".into()));
        assert!(
            new_at.is_some() && new_at < send_at,
            "the message started its own conversation first: {acted:?}"
        );
        assert!(!app.shell_session);
    }

    #[test]
    fn a_shell_line_inside_a_real_conversation_stays_in_it() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 100, 24);
        open_s1(&mut app);
        type_text(&mut app, "!ls", 0);
        app.apply(key(KeyCode::Enter), 0);
        type_text(&mut app, "and now this", 0);
        app.apply(key(KeyCode::Enter), 0);
        assert_eq!(
            sends(&effects),
            vec![Effect::RunShell("ls".into()), a_send("and now this".into())],
            "an ordinary conversation is not left just because a shell ran in it"
        );
    }

    // ---- the seven confirmed defects ---------------------------------------

    use crate::forest::fixtures::{msg, session_row};
    use bough_core::schema::parts::SessionKind;

    /// A drill-in row, the shape `GET /sessions?originId=` answers with.
    fn child(id: &str, origin: &str, kind: SessionKind, busy: bool) -> crate::api::SessionRow {
        let mut row = session_row(id, kind, 1);
        row.session.origin_id = Some(origin.to_string());
        row.session.parent_id = Some(origin.to_string());
        row.busy = busy;
        row
    }

    /// DEFECT 1. `GET /sessions` excludes the collapsing kinds by design, so a
    /// running subagent reached this client on no wire at all: the rail stayed
    /// blank while the server said it was busy.
    #[test]
    fn a_running_subagent_arrives_on_the_drill_in_and_takes_a_rail_row() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);

        // The plain listing is what it has always been: the root alone.
        app.apply(
            Action::Sessions(vec![session_row("s1", SessionKind::Root, 1)]),
            1,
        );
        assert!(
            app.units().is_empty(),
            "nothing has been drilled into yet, so there is nothing to show"
        );

        app.apply(
            Action::ChildSessions {
                origin_id: "s1".into(),
                rows: vec![child("sub-1", "s1", SessionKind::Subagent, true)],
            },
            2,
        );
        let units = app.units();
        assert_eq!(
            units.len(),
            1,
            "the busy subagent is a live unit: {units:?}"
        );
        assert_eq!(
            units[0].kind,
            crate::store::selectors::LiveUnitKind::Subagent
        );
        assert_eq!(units[0].id, "sub-1");
        // …and the status bar's own count agrees with the rail, rather than
        // reading zero while a row is on screen.
        assert_eq!(app.meter().agents, Some(1));
    }

    /// …and the poll ASKS for it. Without this effect the drill-in never
    /// arrives and the fix above is unreachable in the running client.
    #[test]
    fn the_rails_beat_asks_for_the_drill_in_beside_the_listing() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        effects.borrow_mut().clear();
        for i in 0..=POLL_TICKS {
            app.apply(Action::Tick, 100 + i as i64);
        }
        let asked = effects.borrow().clone();
        assert!(
            asked.contains(&Effect::LoadChildSessions("s1".into())),
            "the only wire a subagent arrives on: {asked:?}"
        );
    }

    /// A FINISHED subagent must reach the transcript's branch-card feed too —
    /// same list, same fix.
    #[test]
    fn a_finished_subagent_is_still_a_child_of_the_open_conversation() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(
            Action::ChildSessions {
                origin_id: "s1".into(),
                rows: vec![child("sub-1", "s1", SessionKind::Subagent, false)],
            },
            2,
        );
        assert!(
            app.units().is_empty(),
            "a finished agent is not LIVE work — it belongs to the card feed"
        );
        assert!(
            app.panel
                .children_by_origin
                .get("s1")
                .is_some_and(|rows| rows.iter().any(|r| r.session.id == "sub-1")),
            "…and the tree can still find it"
        );
    }

    /// The drill-in for one origin must not be erased by the drill-in for
    /// another — the tree fetches per expanded row.
    #[test]
    fn one_origins_children_do_not_clobber_anothers() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        app.apply(
            Action::ChildSessions {
                origin_id: "a".into(),
                rows: vec![child("sub-a", "a", SessionKind::Subagent, true)],
            },
            1,
        );
        app.apply(
            Action::ChildSessions {
                origin_id: "b".into(),
                rows: vec![child("sub-b", "b", SessionKind::Subagent, true)],
            },
            2,
        );
        assert_eq!(app.panel.children_by_origin.len(), 2);
        // …and an EMPTY answer is remembered as an answer, so the expand does
        // not re-ask forever.
        app.apply(
            Action::ChildSessions {
                origin_id: "c".into(),
                rows: Vec::new(),
            },
            3,
        );
        assert!(app.panel.children_by_origin.contains_key("c"));
    }

    /// DEFECT 3. A send that never landed left an ordinary `you` bubble in the
    /// transcript forever, between two real turns, for a message the server
    /// never received.
    #[test]
    fn a_send_that_never_landed_takes_its_bubble_back_out() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(Action::Thread(vec![msg("m1", Role::User, "real")]), 1);
        type_text(&mut app, "hello while dead", 2);
        app.apply(key(KeyCode::Enter), 3);
        assert_eq!(app.thread.len(), 2, "the optimistic echo is on screen");
        let local = app.thread.last().unwrap().id.clone();
        assert!(local.starts_with(LOCAL_ID_PREFIX));

        app.apply(
            Action::SendFailed {
                local_id: local,
                text: "hello while dead".into(),
            },
            4,
        );
        assert_eq!(
            app.thread.len(),
            1,
            "a bubble the server never saw is a lie: {:?}",
            app.thread
        );
        assert_eq!(app.thread[0].id, "m1", "the REAL turn is untouched");
        // …and the words come back rather than being lost with it.
        assert_eq!(app.draft, "hello while dead");
        assert_eq!(app.last_send_at, None, "nothing left to take back");
    }

    /// …and a failure that arrives AFTER the snapshot reconciled the id must
    /// not delete a real message.
    #[test]
    fn a_late_send_failure_over_a_reconciled_id_removes_nothing() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        type_text(&mut app, "hi", 1);
        app.apply(key(KeyCode::Enter), 2);
        // The server's own name for it arrives first.
        app.apply(Action::Thread(vec![msg("m9", Role::User, "hi")]), 3);
        app.apply(
            Action::SendFailed {
                local_id: format!("{LOCAL_ID_PREFIX}1"),
                text: "hi".into(),
            },
            4,
        );
        assert_eq!(app.thread.len(), 1);
        assert_eq!(app.thread[0].id, "m9");
        assert_eq!(app.draft, "", "nothing was taken back, so nothing returns");
    }

    /// DEFECT 4. A turn that died with the server left the spinner counting up
    /// forever; nothing reconciled when the stream came back.
    #[test]
    fn the_stream_coming_back_re_reads_what_is_actually_running() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(Action::Connected(true), 1);
        // A turn is in flight, then the server dies under it.
        app.apply(
            Action::Thread(vec![Message {
                pending: true,
                ..msg("m1", Role::Supervisor, "")
            }]),
            2,
        );
        assert!(app.busy(), "the spinner is running");
        app.apply(Action::Connected(false), 3);
        assert!(app.busy(), "…and a dropped stream alone cannot end a turn");

        effects.borrow_mut().clear();
        app.apply(Action::Connected(true), 4);
        let asked = effects.borrow().clone();
        assert!(
            asked.contains(&Effect::ReloadThread),
            "the server marks orphaned turns at boot, so its snapshot is the truth: {asked:?}"
        );

        // …and that snapshot is what stops the spinner.
        app.apply(Action::Thread(vec![msg("m1", Role::Supervisor, "died")]), 5);
        assert!(!app.busy(), "the turn is over and the screen says so");
    }

    /// A stream that was never down must not re-fetch on every frame.
    #[test]
    fn a_stream_that_stayed_up_reconciles_nothing() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(Action::Connected(true), 1);
        effects.borrow_mut().clear();
        app.apply(Action::Connected(true), 2);
        assert!(!effects.borrow().contains(&Effect::ReloadThread));
    }

    /// DEFECT 5. A notice is a flash, not a fixture.
    #[test]
    fn a_notice_expires_and_does_not_ride_into_the_next_conversation() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(Action::Notice("something went wrong".into()), 1_000);
        app.apply(Action::Tick, 2_000);
        assert_eq!(
            app.notice.as_deref(),
            Some("something went wrong"),
            "well inside the window"
        );
        app.apply(
            Action::Tick,
            1_000 + crate::store::shell::NOTICE_TTL_MS as i64,
        );
        assert_eq!(app.notice, None, "ten seconds is the whole life of one");
    }

    /// …and typing retracts the armed-quit row, which promised something the
    /// next `^c` would no longer do.
    #[test]
    fn typing_after_ctrl_c_retracts_what_the_confirm_said() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(ctrl('c'), 1);
        assert_eq!(app.notice.as_deref(), Some(QUIT_CONFIRM));
        assert!(app.quit_armed);
        type_text(&mut app, "x", 2);
        assert!(!app.quit_armed, "typing disarms the quit");
        assert_eq!(app.notice, None, "…and the row must go with the confirm");
    }

    /// DEFECT 6a. `/new` kept the previous conversation's `$cost`, `% ctx left`
    /// and `← back` — a status bar describing a conversation no longer on it.
    #[test]
    fn a_new_conversation_drops_the_old_ones_status_bar() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        let mut row = session_row("s1", SessionKind::Root, 1);
        row.session.origin_id = Some("root-0".into());
        app.apply(
            Action::SessionMeta(Box::new(SessionMeta {
                session: row.session,
                usage: crate::api::SnapshotUsage {
                    totals: bough_core::types::UsageTotals {
                        cost_usd: 1.25,
                        ..Default::default()
                    },
                    tree: bough_core::types::UsageTotals {
                        cost_usd: 1.25,
                        ..Default::default()
                    },
                },
                effective_model: Some("openai/gpt-5.6-luna".into()),
                context_limit: Some(200_000),
                primed_tags: Vec::new(),
                project_rules: Vec::new(),
            })),
            2,
        );
        assert!(app.meter().out, "there IS somewhere to go back to");
        assert_eq!(app.effective_model.as_deref(), Some("openai/gpt-5.6-luna"));

        app.apply(Action::Run(Command::SessionNew, String::new()), 3);
        assert!(app.session.is_none());
        assert!(app.usage.is_none());
        assert_eq!(app.effective_model, None);
        assert_eq!(app.context_limit, None);
        let bar = app.meter();
        assert!(!bar.out, "`← back` pointed at the conversation you left");
        assert_eq!(bar.cost_usd, None);
        assert_eq!(bar.context_limit, None);
    }

    /// DEFECT 7. A streaming message is already in the thread with empty parts,
    /// so the arriving text never changed the LENGTH — and the tree went on
    /// printing `bough (no text)` over a turn full of it.
    #[test]
    fn text_arriving_into_a_streaming_message_reaches_the_tree() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        app.apply(Action::Thread(vec![msg("m1", Role::Supervisor, "")]), 1);
        assert_eq!(
            app.panel.threads.get("s1").map(|t| t.len()),
            Some(1),
            "the empty shell is mirrored"
        );
        // The same COUNT, different content — exactly the case the old
        // length-compare could not see.
        app.apply(
            Action::Thread(vec![msg("m1", Role::Supervisor, "here is the answer")]),
            2,
        );
        let mirrored = app.panel.threads.get("s1").expect("mirrored");
        assert_eq!(mirrored.len(), 1);
        assert!(
            !mirrored[0].parts.is_empty(),
            "the tree must not say `(no text)` over a turn that has text"
        );
    }

    /// ← is the door out of a drilled-into agent, and it was painted on a wall:
    /// the `?` overlay printed the binding, the guard it needs was never fed,
    /// and no arm answered the command.
    #[test]
    fn left_leaves_a_subagent_for_the_session_that_spawned_it() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        app.apply(Action::SessionOpened("sub-1".into()), 1);
        let mut row = session_row("sub-1", SessionKind::Subagent, 1);
        row.session.origin_id = Some("s1".into());
        app.apply(
            Action::SessionMeta(Box::new(SessionMeta {
                session: row.session,
                usage: crate::api::SnapshotUsage {
                    totals: Default::default(),
                    tree: Default::default(),
                },
                effective_model: None,
                context_limit: None,
                primed_tags: Vec::new(),
                project_rules: Vec::new(),
            })),
            2,
        );
        assert!(app.meter().out, "the `← back` chip is drawn");
        effects.borrow_mut().clear();
        app.apply(key(KeyCode::Left), 3);
        assert!(
            effects.borrow().contains(&Effect::OpenSession("s1".into())),
            "the chip and the key must agree on the destination: {:?}",
            effects.borrow()
        );
    }

    /// …and ← is still the cursor everywhere else. A root conversation has no
    /// spawner, and a draft under the cursor outranks the door.
    #[test]
    fn left_is_still_the_cursor_when_there_is_nowhere_to_go_out_to() {
        let (effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        type_text(&mut app, "abc", 1);
        effects.borrow_mut().clear();
        app.apply(key(KeyCode::Left), 2);
        assert_eq!(app.cursor, 2, "the caret moved, nothing was opened");
        assert!(effects.borrow().is_empty());
    }

    /// …and the tick that retires it must actually be asked for. On an idle
    /// screen nothing else moves, so a notice nobody is animating for is a
    /// notice that never expires.
    #[test]
    fn a_pending_notice_keeps_the_screen_animating_until_it_retires() {
        let (_effects, sink) = scripted();
        let mut app = App::new(TuiOptions::default(), sink, 80, 24);
        open_s1(&mut app);
        assert!(!app.animating(), "an idle screen is idle");
        app.apply(Action::Notice("something went wrong".into()), 1_000);
        assert!(app.animating(), "…until there is a row waiting to go");
        app.apply(
            Action::Tick,
            1_000 + crate::store::shell::NOTICE_TTL_MS as i64,
        );
        assert_eq!(app.notice, None);
        assert!(!app.animating(), "…and idle again once it has gone");
    }
}
