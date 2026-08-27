//! Invariant: the composer decides SEND vs COMMAND on the buffer alone, before anything is
//! dispatched. A line that begins with the prefix never reaches an agent, and a line that does not
//! never reaches `ctx.commands` — the two paths cannot cross (V5).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui_textarea::{Input, Key, TextArea};

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
}

impl Composer {
    /// An empty composer.
    pub fn new(cfg: &TuiConfig) -> Composer {
        let mut area = TextArea::default();
        area.set_placeholder_text("message, or / for a command");
        Composer {
            area,
            max_lines: cfg.composer_max_lines,
            prefix: '/',
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

    /// Enter sends; Alt+Enter and Shift+Enter (on terminals that report it) insert a newline.
    pub fn on_key(&mut self, key: KeyEvent) -> ComposerAction {
        if key.kind == KeyEventKind::Release {
            return ComposerAction::None;
        }
        let newline = key.modifiers.contains(KeyModifiers::ALT)
            || key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Enter if newline => {
                self.area.insert_newline();
                ComposerAction::None
            }
            KeyCode::Enter => {
                let text = self.text();
                if text.trim().is_empty() {
                    return ComposerAction::None;
                }
                self.area.select_all();
                self.area.cut();
                self.area.clear();
                if is_command(&text, self.prefix) {
                    ComposerAction::Command(text)
                } else {
                    ComposerAction::Send(text)
                }
            }
            KeyCode::Esc => {
                if self.text().is_empty() {
                    ComposerAction::None
                } else {
                    self.area.clear();
                    ComposerAction::Cleared
                }
            }
            _ => {
                self.area.input(input_of(key));
                ComposerAction::None
            }
        }
    }

    /// Clear the buffer. The SHELL calls this, and only after `ctx.commands` resolved the name:
    /// a command line that matched nothing keeps its text (phase ux1 §2.3, B3).
    pub fn clear(&mut self) {
        self.area.select_all();
        self.area.cut();
        self.area.clear();
    }

    /// Arm "a second Enter sends this line as a message" after a command miss. Any edit disarms
    /// it. This is the way out of B3: the text stays and the user is told how to send it.
    pub fn arm_send_as_message(&mut self) {
        todo!("WP-2")
    }

    /// Whether the next unchanged Enter sends the line as a message.
    pub fn send_as_message_armed(&self) -> bool {
        todo!("WP-2")
    }

    /// Map a click's column/row to a caret offset (minor 33).
    pub fn caret_at(&mut self, col: u16, row: u16, area: Rect) {
        let _ = (col, row, area);
        todo!("WP-2")
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
        cx.frame.render_widget(&self.area, area);
    }

    /// Replace the buffer.
    pub fn set_text(&mut self, text: &str) {
        self.area.select_all();
        self.area.cut();
        self.area.clear();
        self.area.insert_str(text);
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

    fn typed(c: &mut Composer, text: &str) {
        for ch in text.chars() {
            c.on_key(key(KeyCode::Char(ch)));
        }
    }

    #[test]
    fn enter_on_plain_text_is_a_send_and_clears_the_buffer() {
        let mut c = Composer::new(&cfg());
        typed(&mut c, "hello");
        assert_eq!(
            c.on_key(key(KeyCode::Enter)),
            ComposerAction::Send("hello".to_string())
        );
        assert!(c.is_empty());
    }

    #[test]
    fn enter_on_a_prefixed_line_is_a_command_and_never_a_send() {
        let mut c = Composer::new(&cfg());
        typed(&mut c, "/help");
        assert_eq!(
            c.on_key(key(KeyCode::Enter)),
            ComposerAction::Command("/help".to_string())
        );
    }

    #[test]
    fn a_doubled_prefix_is_a_message_that_starts_with_a_slash() {
        assert!(!is_command("//not a command", '/'));
        assert!(is_command("/help", '/'));
        assert!(!is_command("help", '/'));
        assert!(!is_command("", '/'));
    }

    #[test]
    fn esc_clears_a_non_empty_buffer_and_leaves_an_empty_one_to_the_shell() {
        let mut c = Composer::new(&cfg());
        typed(&mut c, "draft");
        assert_eq!(c.on_key(key(KeyCode::Esc)), ComposerAction::Cleared);
        assert_eq!(c.on_key(key(KeyCode::Esc)), ComposerAction::None);
    }

    #[test]
    fn the_height_grows_with_the_lines_and_stops_at_the_configured_maximum() {
        let mut c = Composer::new(&cfg());
        assert_eq!(c.height(20), 1);
        for _ in 0..20 {
            c.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        }
        assert_eq!(c.height(20), cfg().composer_max_lines);
    }
}
