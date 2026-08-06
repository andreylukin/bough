//! The TUI event loop (wave-1 port of `src/tui/components/App.tsx` +
//! `src/tui/main.tsx` responsibilities, row 1.39).
//!
//! Concurrency shape (ARCHITECTURE §5): the crossterm `EventStream`, the SSE
//! task and the timer tasks all post [`Action`]s over ONE mpsc; the reducer
//! ([`App::apply`]) runs on the single loop task and stays pure of I/O — every
//! outbound call is an [`Effect`] handed to the injected [`Transport`], so the
//! whole loop is scriptable in tests with no terminal and no server attached.
//!
//! WAVE-1 SCOPE (kept honest, per PORT_PLAN row 1.39 and spec §8 v1 cut):
//! chat mode only — no panel, no rail, no job view, no help overlay yet; ghost
//! absent (cheap tier is `None`); FALLBACK palette only; mouse = wheel scroll
//! only; the `!` shell and `/` commands answer "not wired into this client"
//! and keep the draft rather than billing the model with a command. The
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
use bough_core::schema::parts::{Message, Part, Role};
use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
    MouseEventKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::components::chat::{render_chat, ChatProps, CHAT_PLACEHOLDER};
use crate::components::composer::{composer_height, render_composer, ComposerProps};
use crate::components::status::{render_status, ChatMeter};
use crate::components::WARN;

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
}

/// Outbound calls. The loop never does I/O itself; the transport does.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// POST the draft as a user message.
    Send(String),
    /// POST /sessions/:id/interrupt.
    Interrupt,
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
        }
    }

    /// A turn is in flight iff any message is pending (store.ts::isBusy).
    pub fn busy(&self) -> bool {
        self.thread.iter().any(|m| m.pending)
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
            }
            Action::Connected(up) => self.connected = up,
            Action::SessionOpened(id) => self.session_id = Some(id),
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

    fn on_mouse(&mut self, m: MouseEvent) {
        match m.kind {
            MouseEventKind::ScrollUp => self.scroll_by(WHEEL_ROWS as isize),
            MouseEventKind::ScrollDown => self.scroll_by(-(WHEEL_ROWS as isize)),
            _ => {} // drag-select and click-to-fold land with the mouse port (spec §8)
        }
    }

    fn scroll_by(&mut self, delta: isize) {
        let max = self.transcript_lines().len().saturating_sub(1);
        let next = self.scroll_off as isize + delta;
        self.scroll_off = next.clamp(0, max as isize) as usize;
    }

    fn on_key(&mut self, k: KeyEvent, now_ms: i64) {
        if k.kind == KeyEventKind::Release {
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
    }

    fn delete_back(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let at = self.byte_at(self.cursor - 1);
        self.draft.remove(at);
        self.cursor -= 1;
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
        if let Some(name) = lone_slash_word(&text) {
            self.notice = Some(format!(
                "there is no /{name} — slash commands are not wired into this client yet"
            ));
            return; // draft kept
        }
        self.clear_draft();
        self.scroll_off = 0;
        self.notice = None;
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
            EventType::AskQuestion => {}
            EventType::JobSpawned => {}
            EventType::JobExited => {}
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

    pub fn draw(&self, area: Rect, buf: &mut Buffer) {
        let cols = area.width.max(20);
        let rows = area.height.max(8);
        let lines = self.transcript_lines();
        let busy = self.busy();
        // App.tsx: composerRows = min(8, max(3, rows/4)).
        let composer_rows = ((rows as usize) / 4).clamp(3, 8);
        let input_h =
            composer_height(&self.draft, "", busy, cols, composer_rows, 0) as u16;
        let rail_h = 0u16; // the rail lands in wave 2
        let chat_h = (rows as i32 - 1 - rail_h as i32 - input_h as i32 - 1).max(1) as u16;

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
            Rect { x: area.x, y: area.y + 1, width: cols, height: chat_h },
            buf,
        );

        render_composer(
            &ComposerProps {
                input: &self.draft,
                cursor: self.cursor,
                busy,
                width: cols,
                max_rows: composer_rows,
                ghost: "", // ghost absent by contract in v1
                attachments: &[],
                keyboard_owner: None,
            },
            Rect { x: area.x, y: area.y + 1 + chat_h, width: cols, height: input_h },
            buf,
        );

        render_status(
            &ChatMeter {
                workspace: self.options.workspace.clone(),
                help: true,
                ..Default::default()
            },
            Rect {
                x: area.x,
                y: (area.y + 1 + chat_h + input_h).min(area.y + rows - 1),
                width: cols,
                height: 1,
            },
            buf,
        );
    }
}

fn first_text(msg: &Message) -> Option<&str> {
    msg.parts.iter().find_map(|p| match p {
        Part::Text { text } => Some(text.as_str()),
        _ => None,
    })
}

/// keys.ts::unknownCommand's trigger shape: a LONE `/word` draft.
fn lone_slash_word(draft: &str) -> Option<&str> {
    let trimmed = draft.trim();
    let rest = trimmed.strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }
    let ok = rest
        .chars()
        .enumerate()
        .all(|(i, c)| {
            if i == 0 {
                c.is_ascii_alphanumeric()
            } else {
                c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-')
            }
        });
    ok.then_some(rest)
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
        // when nothing is live).
        if is_tick && !app.busy() {
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
        assert_eq!(effects.borrow().as_slice(), &[Effect::Send("add a test".into())]);
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
        app.apply(key(KeyCode::Esc), 2_000);
        assert_eq!(
            effects.borrow().as_slice(),
            &[Effect::Send("add a test".into()), Effect::Interrupt]
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
        assert_eq!(effects.borrow().as_slice(), &[Effect::Interrupt]);
        assert_eq!(app.draft, "draft2", "the draft survives a stop");
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
        type_text(&mut app, "/model", 40);
        app.apply(key(KeyCode::Enter), 50);
        assert!(effects.borrow().is_empty());
        assert!(app.notice.as_deref().unwrap().contains("there is no /model"), "{:?}", app.notice);

        // A message that merely BEGINS with a command is still a message.
        app.apply(key(KeyCode::Esc), 60);
        app.apply(key(KeyCode::Esc), 70);
        type_text(&mut app, "/model is the wrong word here", 80);
        app.apply(key(KeyCode::Enter), 90);
        assert_eq!(
            effects.borrow().as_slice(),
            &[Effect::Send("/model is the wrong word here".into())]
        );
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
