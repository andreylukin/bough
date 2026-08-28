//! Invariant: the composer decides SEND vs COMMAND on the buffer alone, before anything is
//! dispatched. A line that begins with the prefix never reaches an agent, and a line that does not
//! never reaches `ctx.commands` — the two paths cannot cross (V5).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui_textarea::{CursorMove, Input, Key, TextArea};

use crate::draft::{kill_to_line_start, SentHistory};
use crate::pane::RenderCx;
use crate::TuiConfig;

/// What a key did to the composer.
#[derive(Clone, Debug, PartialEq)]
pub enum ComposerAction {
    None,
    /// Enter on a non-empty buffer that does not start with the command prefix.
    Send(String),
    /// Enter on a buffer that starts with the command prefix.
    Command(String),
    /// `Ctrl+U` on a non-empty line. Esc no longer produces this (phase ux1 §2.3, V3): the draft
    /// is never destroyed by anything except an explicit clear.
    Cleared,
    /// A newline was inserted — by `Shift+Enter`/`Alt+Enter`, or by an Enter the shell told the
    /// composer was part of a paste burst (B4).
    Newline,
}

/// The message box, over `ratatui-textarea` (P3-D1).
pub struct Composer {
    area: TextArea<'static>,
    max_lines: u16,
    /// The command prefix, learned from `ctx.commands` once the shell has it. `/` until then, so a
    /// composer built before the registry still classifies the same way.
    prefix: char,
    /// Set by the shell after a command MISS: the text stayed, and the next unchanged Enter sends
    /// it as a message (B3). Any edit disarms it.
    armed: bool,
    /// Sent-message recall over an empty draft (M20).
    history: SentHistory,
}

/// The prompt glyph, two cells wide with its trailing space.
const PROMPT: &str = "\u{203a} ";

/// One clickable chip at the right of the composer band (the TUI brief, D7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChipKind {
    /// `to: sol ▾` — which lane the message goes to; a click moves to the next lane.
    Lane,
    /// `send ⏎` — the same thing Enter does.
    Send,
    /// `stop ⎋` — the same thing Esc does while a turn runs.
    Stop,
}

/// A chip's text and its columns within the band (`x0..x1`, band-relative).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    pub kind: ChipKind,
    pub text: String,
    pub x0: u16,
    pub x1: u16,
}

/// Bands narrower than this get no chips: the field is what a narrow band is for.
pub const CHIPS_MIN_WIDTH: u16 = 60;

/// PURE: the chips a band of `width` columns shows, right-aligned, one space apart (D7). `lane`
/// is the focused lane's name (no lane, no `to:` chip); `running` swaps `send ⏎` for `stop ⎋`.
/// The same function decides where a click lands, so the drawing and the hit test cannot drift.
pub fn chips(width: u16, lane: Option<&str>, running: bool) -> Vec<Chip> {
    if width < CHIPS_MIN_WIDTH {
        return Vec::new();
    }
    let mut texts: Vec<(ChipKind, String)> = Vec::new();
    if let Some(lane) = lane {
        texts.push((ChipKind::Lane, format!(" to: {lane} \u{25be} ")));
    }
    if running {
        texts.push((ChipKind::Stop, " stop \u{238b} ".to_string()));
    } else {
        texts.push((ChipKind::Send, " send \u{23ce} ".to_string()));
    }
    // Lay out from the right edge, leaving one column of band after the last chip.
    let mut x1 = width.saturating_sub(1);
    let mut out = Vec::new();
    for (kind, text) in texts.into_iter().rev() {
        let w = text.chars().count() as u16;
        let x0 = x1.saturating_sub(w);
        out.push(Chip { kind, text, x0, x1 });
        x1 = x0.saturating_sub(1);
    }
    out.reverse();
    out
}

/// PURE: the chip under band-relative column `col`, if any.
pub fn chip_at(chips: &[Chip], col: u16) -> Option<&Chip> {
    chips.iter().find(|c| col >= c.x0 && col < c.x1)
}

impl Composer {
    /// An empty composer.
    pub fn new(cfg: &TuiConfig) -> Composer {
        let mut area = TextArea::default();
        area.set_placeholder_text(Composer::placeholder());
        Composer {
            area,
            max_lines: cfg.composer_max_lines,
            prefix: '/',
            armed: false,
            history: SentHistory::new(cfg.history_cap),
        }
    }

    /// Adopt the registry's prefix. Called once, when the shell resolves `ctx.commands`.
    pub fn set_prefix(&mut self, prefix: char) {
        self.prefix = prefix;
    }

    /// The prefix a line must start with to be a command.
    pub fn prefix(&self) -> char {
        self.prefix
    }

    /// Enter sends; Alt+Enter and Shift+Enter (on terminals that report it) insert a newline;
    /// and an Enter the shell has told us is part of a paste burst is a newline too (B4).
    ///
    /// `in_burst` comes from `PasteBurst::on_key(now)`, which `run::on_key` calls BEFORE this
    /// (phase ux1 §2.3 sequencing rule). Nothing here ever destroys the draft: Esc does nothing,
    /// a command line keeps its text until the shell resolves the name, and only Ctrl+U and a
    /// completed Send clear.
    pub fn on_key(&mut self, key: KeyEvent, in_burst: bool) -> ComposerAction {
        if key.kind == KeyEventKind::Release {
            return ComposerAction::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let newline = key.modifiers.contains(KeyModifiers::ALT)
            || key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Enter if newline || in_burst => {
                self.armed = false;
                self.area.insert_newline();
                ComposerAction::Newline
            }
            KeyCode::Enter => {
                let text = self.text();
                if text.trim().is_empty() {
                    return ComposerAction::None;
                }
                // A line the shell already told us matched no command: the second, unchanged
                // Enter is the way out — send exactly what is on the screen (B3).
                if self.armed {
                    self.armed = false;
                    self.clear();
                    self.history.push(&text);
                    return ComposerAction::Send(text);
                }
                if is_command(&text, self.prefix) {
                    // The buffer is NOT cleared here. `clear()` is the shell's to call, and only
                    // once `ctx.commands` resolved the name.
                    ComposerAction::Command(text)
                } else {
                    self.clear();
                    let sent = undouble_prefix(&text, self.prefix);
                    self.history.push(&sent);
                    ComposerAction::Send(sent)
                }
            }
            // Esc destroys nothing (V3). What Esc DOES mean — interrupt, dismiss an overlay —
            // is the shell's keymap, and it never reaches the composer.
            KeyCode::Esc => ComposerAction::None,
            KeyCode::Char('u') if ctrl => {
                let (row, col) = {
                    let c = self.area.cursor();
                    (c.0, c.1)
                };
                let line = self.area.lines().get(row).cloned().unwrap_or_default();
                if col == 0 {
                    return ComposerAction::None;
                }
                let (kept, caret) = kill_to_line_start(&line, col);
                self.armed = false;
                self.replace_line(row, &kept, caret);
                ComposerAction::Cleared
            }
            // Over an EMPTY draft, Up/Down walk the sent-message history (M20). Mid-walk they
            // keep walking, so the user can come back down to the draft they were holding.
            KeyCode::Up if self.text().is_empty() || self.history.is_walking() => {
                let draft = self.text();
                if let Some(text) = self.history.prev(&draft) {
                    self.set_text_keeping_history(&text);
                }
                ComposerAction::None
            }
            KeyCode::Down if self.history.is_walking() => {
                if let Some(text) = self.history.next() {
                    self.set_text_keeping_history(&text);
                }
                ComposerAction::None
            }
            _ => {
                let modified = self.area.input(input_of(key));
                if modified {
                    // Any edit disarms the send-as-message offer and ends a history walk: the
                    // line on the screen is the user's again.
                    self.armed = false;
                    self.history.reset();
                }
                ComposerAction::None
            }
        }
    }

    /// Record a line the shell sent on the composer's behalf (a queued message, a resend), so
    /// `↑` recalls it too.
    pub fn remember_sent(&mut self, text: &str) {
        self.history.push(text);
    }

    /// Clear the buffer. The SHELL calls this, and only after `ctx.commands` resolved the name:
    /// a command line that matched nothing keeps its text (phase ux1 §2.3, B3).
    pub fn clear(&mut self) {
        self.armed = false;
        self.history.reset();
        self.area.select_all();
        self.area.cut();
        self.area.clear();
    }

    /// Arm "a second Enter sends this line as a message" after a command miss. Any edit disarms
    /// it. This is the way out of B3: the text stays and the user is told how to send it.
    pub fn arm_send_as_message(&mut self) {
        self.armed = !self.text().trim().is_empty();
    }

    /// Whether the next unchanged Enter sends the line as a message.
    pub fn send_as_message_armed(&self) -> bool {
        self.armed
    }

    /// Map a click's column/row to a caret offset (minor 33).
    pub fn caret_at(&mut self, col: u16, row: u16, area: Rect) {
        let row = row.saturating_sub(area.y) as usize;
        let col = col.saturating_sub(area.x) as usize;
        let last = self.area.lines().len().saturating_sub(1);
        let row = row.min(last);
        let width = self.area.lines()[row].chars().count();
        let col = col.min(width);
        self.area
            .move_cursor(CursorMove::Jump(row as u16, col as u16));
    }

    /// The placeholder, now a sentence rather than a fragment.
    pub fn placeholder() -> &'static str {
        "Type a message, or / for a command"
    }

    /// Bracketed paste. A pasted newline is text, never a send.
    pub fn on_paste(&mut self, text: &str) {
        self.area.insert_str(text);
    }

    /// Rows the composer wants, clamped to `max` and to the configured maximum.
    pub fn height(&self, max: u16) -> u16 {
        let lines = self.area.lines().len().max(1) as u16;
        lines.min(self.max_lines).min(max.max(1)).max(1)
    }

    /// Draw into the composer's rectangle.
    pub fn render(&self, cx: &mut RenderCx<'_>) {
        let area = cx.area;
        self.render_at(cx, area);
    }

    /// Draw into an explicit rectangle. The shell owns the composer's slot, so it knows the
    /// rectangle before a `RenderCx` for it exists.
    pub fn render_at(&self, cx: &mut RenderCx<'_>, area: Rect) {
        // The band and the prompt glyph (visual audit F9): the composer is the one row that is
        // an INPUT, and nothing used to say so — a dim placeholder on the same ground as the
        // command output directly above it. The band is `field_bg`; the glyph is the accent, so
        // the eye finds the row the same way it finds the focused pane's ring.
        let theme = *cx.theme();
        let band = Style::default().bg(theme.field_bg);
        cx.frame.render_widget(Paragraph::new("").style(band), area);
        if area.width < 3 {
            cx.frame.render_widget(&self.area, area);
            return;
        }
        cx.frame.render_widget(
            Paragraph::new(Line::styled(
                PROMPT,
                Style::default().fg(theme.accent).bg(theme.field_bg),
            )),
            Rect { width: 2, ..area },
        );
        // The chips (D7): drawn on the band's LAST row, right-aligned, on the selection ground so
        // they read as things to press. The field keeps every column to their left.
        let chips = chips(area.width, cx.view.focused_name.as_deref(), cx.view.running);
        let field_w = chips
            .first()
            .map(|c| c.x0.saturating_sub(1))
            .unwrap_or(area.width)
            .saturating_sub(2);
        let mut field = self.area.clone();
        field.set_style(band);
        field.set_cursor_line_style(band);
        field.set_placeholder_style(Style::default().fg(theme.dim).bg(theme.field_bg));
        cx.frame.render_widget(
            &field,
            Rect {
                x: area.x + 2,
                width: field_w,
                ..area
            },
        );
        let last_row = area.y + area.height.saturating_sub(1);
        for chip in &chips {
            let style = Style::default().fg(theme.fg).bg(theme.sel_bg);
            cx.frame.render_widget(
                Paragraph::new(Line::styled(chip.text.clone(), style)),
                Rect {
                    x: area.x + chip.x0,
                    y: last_row,
                    width: chip.x1 - chip.x0,
                    height: 1,
                },
            );
        }
    }

    /// Replace the buffer.
    pub fn set_text(&mut self, text: &str) {
        self.history.reset();
        self.set_text_keeping_history(text);
    }

    /// Replace the buffer without ending a history walk (the walk is what is writing it).
    fn set_text_keeping_history(&mut self, text: &str) {
        self.armed = false;
        self.area.select_all();
        self.area.cut();
        self.area.clear();
        self.area.insert_str(text);
    }

    /// Replace one line and put the caret at `caret` characters into it.
    fn replace_line(&mut self, row: usize, line: &str, caret: usize) {
        let mut lines: Vec<String> = self.area.lines().to_vec();
        if row < lines.len() {
            lines[row] = line.to_string();
        }
        self.area.select_all();
        self.area.cut();
        self.area.clear();
        self.area.insert_str(lines.join("\n"));
        self.area
            .move_cursor(CursorMove::Jump(row as u16, caret as u16));
    }

    /// The buffer.
    pub fn text(&self) -> String {
        self.area.lines().join("\n")
    }

    /// Whether there is anything to send.
    pub fn is_empty(&self) -> bool {
        self.text().is_empty()
    }
}

/// PURE: whether a line is a command line. A DOUBLED prefix is literal text (`//x` is a message
/// that starts with a slash), which is the same rule `commands::parse` applies.
pub fn is_command(line: &str, prefix: char) -> bool {
    let mut chars = line.chars();
    chars.next() == Some(prefix) && chars.next() != Some(prefix)
}

/// PURE: a line that begins with a DOUBLED prefix is a message that starts with the prefix, and
/// the escape is not part of what the user meant to say — `//x` sends `/x` (B3's escape hatch).
pub fn undouble_prefix(line: &str, prefix: char) -> String {
    let mut chars = line.chars();
    if chars.next() == Some(prefix) && chars.next() == Some(prefix) {
        return line.chars().skip(1).collect();
    }
    line.to_string()
}

/// crossterm's key event as the textarea's backend-agnostic input. Written out rather than taken
/// from the crate's `From` impl so the two crossterm versions can never silently diverge.
fn input_of(key: KeyEvent) -> Input {
    let k = match key.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Enter => Key::Enter,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Tab => Key::Tab,
        KeyCode::Delete => Key::Delete,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Esc => Key::Esc,
        KeyCode::F(n) => Key::F(n),
        _ => Key::Null,
    };
    Input {
        key: k,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> TuiConfig {
        crate::test_config()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press(c: &mut Composer, code: KeyCode) -> ComposerAction {
        c.on_key(key(code), false)
    }

    fn typed(c: &mut Composer, text: &str) {
        for ch in text.chars() {
            c.on_key(key(KeyCode::Char(ch)), false);
        }
    }

    #[test]
    fn enter_on_plain_text_is_a_send_and_clears_the_buffer() {
        let mut c = Composer::new(&cfg());
        typed(&mut c, "hello");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Send("hello".to_string())
        );
        assert!(c.is_empty());
    }

    #[test]
    fn enter_on_a_prefixed_line_is_a_command_and_keeps_the_text() {
        let mut c = Composer::new(&cfg());
        typed(&mut c, "/help");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Command("/help".to_string())
        );
        assert_eq!(
            c.text(),
            "/help",
            "the shell clears it, and only on a resolved dispatch (B3)"
        );
    }

    #[test]
    fn a_missed_command_keeps_its_text_and_a_second_enter_sends_it_as_a_message() {
        let mut c = Composer::new(&cfg());
        typed(&mut c, "/nonsense");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Command("/nonsense".to_string())
        );
        // The shell looked it up, found nothing, and armed the way out.
        c.arm_send_as_message();
        assert!(c.send_as_message_armed());
        assert_eq!(c.text(), "/nonsense", "nothing was destroyed");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Send("/nonsense".to_string())
        );
        assert!(c.is_empty());
    }

    #[test]
    fn any_edit_disarms_the_send_as_message_offer() {
        let mut c = Composer::new(&cfg());
        typed(&mut c, "/nonsens");
        c.arm_send_as_message();
        typed(&mut c, "e");
        assert!(!c.send_as_message_armed());
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Command("/nonsense".to_string()),
            "the edited line is a command line again"
        );
    }

    #[test]
    fn a_doubled_prefix_sends_one_slash_as_a_message() {
        let mut c = Composer::new(&cfg());
        typed(&mut c, "//x");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Send("/x".to_string())
        );
        assert!(!is_command("//not a command", '/'));
        assert!(is_command("/help", '/'));
        assert!(!is_command("help", '/'));
        assert!(!is_command("", '/'));
        assert_eq!(undouble_prefix("plain", '/'), "plain");
    }

    #[test]
    fn esc_leaves_the_draft_alone() {
        let mut c = Composer::new(&cfg());
        typed(&mut c, "draft");
        assert_eq!(press(&mut c, KeyCode::Esc), ComposerAction::None);
        assert_eq!(c.text(), "draft", "V3: Esc destroys nothing");
    }

    #[test]
    fn ctrl_u_kills_to_the_start_of_the_line() {
        let mut c = Composer::new(&cfg());
        typed(&mut c, "abcdefgh");
        assert_eq!(
            c.on_key(
                KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                false
            ),
            ComposerAction::Cleared
        );
        assert_eq!(c.text(), "", "the whole line, not one character");

        typed(&mut c, "abcdef");
        for _ in 0..3 {
            press(&mut c, KeyCode::Left);
        }
        c.on_key(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            false,
        );
        assert_eq!(c.text(), "def", "everything after the caret survives");
    }

    #[test]
    fn an_enter_inside_a_paste_burst_is_a_newline_and_the_paste_is_one_draft() {
        let mut c = Composer::new(&cfg());
        for line in ["one", "two", "three"] {
            if !c.is_empty() {
                assert_eq!(
                    c.on_key(key(KeyCode::Enter), true),
                    ComposerAction::Newline,
                    "a burst newline is never a send (B4)"
                );
            }
            for ch in line.chars() {
                c.on_key(key(KeyCode::Char(ch)), true);
            }
        }
        assert_eq!(c.text(), "one\ntwo\nthree");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Send("one\ntwo\nthree".to_string()),
            "and one send carries all three lines"
        );
    }

    #[test]
    fn the_same_three_lines_typed_slowly_are_three_sends() {
        let mut c = Composer::new(&cfg());
        let mut sends = Vec::new();
        for line in ["one", "two", "three"] {
            typed(&mut c, line);
            if let ComposerAction::Send(text) = press(&mut c, KeyCode::Enter) {
                sends.push(text);
            }
        }
        assert_eq!(sends, vec!["one", "two", "three"]);
    }

    #[test]
    fn up_and_down_over_an_empty_draft_walk_the_sent_history() {
        let mut c = Composer::new(&cfg());
        typed(&mut c, "one");
        press(&mut c, KeyCode::Enter);
        typed(&mut c, "two");
        press(&mut c, KeyCode::Enter);

        typed(&mut c, "live");
        // A non-empty draft keeps Up as the composer's own cursor.
        press(&mut c, KeyCode::Up);
        assert_eq!(c.text(), "live");

        c.clear();
        press(&mut c, KeyCode::Up);
        assert_eq!(c.text(), "two");
        press(&mut c, KeyCode::Up);
        assert_eq!(c.text(), "one");
        press(&mut c, KeyCode::Down);
        assert_eq!(c.text(), "two");
        press(&mut c, KeyCode::Down);
        assert_eq!(c.text(), "", "the empty draft it was holding came back");
    }

    #[test]
    fn a_history_walk_hands_back_the_draft_it_was_holding() {
        let mut c = Composer::new(&cfg());
        typed(&mut c, "sent");
        press(&mut c, KeyCode::Enter);
        c.set_text("");
        press(&mut c, KeyCode::Up);
        assert_eq!(c.text(), "sent");
        press(&mut c, KeyCode::Down);
        assert_eq!(c.text(), "");
    }

    #[test]
    fn a_click_puts_the_caret_where_the_pointer_is() {
        let mut c = Composer::new(&cfg());
        typed(&mut c, "hello");
        c.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT), false);
        typed(&mut c, "world");
        let area = Rect::new(2, 10, 40, 2);
        c.caret_at(2 + 3, 10, area);
        assert_eq!(c.area.cursor(), (0, 3));
        c.caret_at(2 + 99, 10 + 1, area);
        assert_eq!(c.area.cursor(), (1, 5), "past the end clamps to the line");
        c.caret_at(0, 10 + 40, area);
        assert_eq!(c.area.cursor(), (1, 0), "and past the last row to it");
    }

    #[test]
    fn the_placeholder_is_a_sentence() {
        assert_eq!(
            Composer::placeholder(),
            "Type a message, or / for a command"
        );
        let c = Composer::new(&cfg());
        assert_eq!(c.area.placeholder_text(), Composer::placeholder());
    }

    #[test]
    fn the_height_grows_with_the_lines_and_stops_at_the_configured_maximum() {
        let mut c = Composer::new(&cfg());
        assert_eq!(c.height(20), 1);
        for _ in 0..20 {
            c.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT), false);
        }
        assert_eq!(c.height(20), cfg().composer_max_lines);
    }
}

#[cfg(test)]
mod chip_tests {
    use super::{chip_at, chips, ChipKind, CHIPS_MIN_WIDTH};

    #[test]
    fn chips_sit_at_the_right_edge_in_order_and_swap_send_for_stop_while_running() {
        let idle = chips(80, Some("sol"), false);
        assert_eq!(
            idle.iter().map(|c| c.kind.clone()).collect::<Vec<_>>(),
            vec![ChipKind::Lane, ChipKind::Send]
        );
        assert_eq!(idle[1].x1, 79, "one column of band after the last chip");
        assert_eq!(idle[0].x1 + 1, idle[1].x0, "one column between chips");
        assert!(idle[0].text.contains("to: sol"));
        // The hit test is the same geometry.
        assert_eq!(
            chip_at(&idle, idle[0].x0).map(|c| &c.kind),
            Some(&ChipKind::Lane)
        );
        assert_eq!(
            chip_at(&idle, idle[1].x1 - 1).map(|c| &c.kind),
            Some(&ChipKind::Send)
        );
        assert!(chip_at(&idle, idle[0].x0 - 1).is_none());
        let running = chips(80, Some("sol"), true);
        assert_eq!(running[1].kind, ChipKind::Stop);
        // No lane: no `to:` chip. Narrow: no chips at all.
        assert_eq!(chips(80, None, false).len(), 1);
        assert!(chips(CHIPS_MIN_WIDTH - 1, Some("sol"), false).is_empty());
    }
}
