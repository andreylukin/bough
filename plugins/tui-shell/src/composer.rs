//! Invariant: the composer decides SEND vs COMMAND on the buffer alone, before anything is
//! dispatched. A line that begins with the prefix never reaches an agent, and a line that does not
//! never reaches `ctx.commands` — the two paths cannot cross (V5).

use crossterm::event::KeyEvent;

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
    /// Esc on a non-empty buffer clears it; on an empty one the shell handles it.
    Cleared,
}

/// The message box, over `ratatui-textarea` (P3-D1).
pub struct Composer {
    _private: (),
}

impl Composer {
    /// An empty composer.
    pub fn new(_cfg: &TuiConfig) -> Composer {
        todo!("WP-2")
    }

    /// Enter sends; Alt+Enter and Shift+Enter (on terminals that report it) insert a newline.
    pub fn on_key(&mut self, _key: KeyEvent) -> ComposerAction {
        todo!("WP-2")
    }

    /// Bracketed paste.
    pub fn on_paste(&mut self, _text: &str) {
        todo!("WP-2")
    }

    /// Rows the composer wants, clamped to `max`.
    pub fn height(&self, _max: u16) -> u16 {
        todo!("WP-2")
    }

    /// Draw into the composer's rectangle.
    pub fn render(&self, _cx: &mut RenderCx<'_>) {
        todo!("WP-2")
    }

    /// Replace the buffer.
    pub fn set_text(&mut self, _text: &str) {
        todo!("WP-2")
    }

    /// The buffer.
    pub fn text(&self) -> String {
        todo!("WP-2")
    }
}
