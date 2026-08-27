//! Invariant: the meaning of a key is decided ONCE, by a pure function of the key and a snapshot
//! of shell state, BEFORE anything is dispatched (phase ux1 §2.1). There is no second place that
//! reinterprets a key, so "who has focus" can never silently change what PageUp does (B1, B2).

use std::time::Duration;

use chrono::{DateTime, Utc};
use crossterm::event::KeyEvent;

use crate::pane::PaneId;

/// Where the keyboard is. Exactly one of these is true at any moment, and the frame SHOWS which.
#[derive(Clone, Debug, PartialEq)]
pub enum Focus {
    /// The default, and where every session starts and returns to.
    Composer,
    /// A pane took the keyboard by Tab, Ctrl+F, or a deliberate click on a focusable pane.
    Pane { pane: PaneId },
}

/// What the shell does with one key, decided BEFORE anything is dispatched. PURE and total.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Scroll the transcript, whatever has focus (V2). Positive `delta` moves toward the newest row.
    Scroll { delta: i16 },
    /// `End`: back to the tail.
    JumpLatest,
    /// Tab / BackTab: one step around the focus ring.
    CycleFocus(i32),
    /// Esc while a wake is running.
    Interrupt,
    /// Esc with an overlay open: dismiss the topmost one.
    DismissOverlay,
    /// Ctrl+C: arm, or exit if already armed.
    ExitStep,
    FocusSearch,
    Redraw,
    /// Not the shell's: goes to the composer (`Focus::Composer`) or the focused pane.
    Pass,
}

/// Everything [`action_for`] needs. No handle, no lock: the caller reads the shell once.
#[derive(Clone, Copy, Debug)]
pub struct KeyContext {
    pub focus_is_composer: bool,
    pub draft_is_empty: bool,
    pub running: bool,
    pub overlay_open: bool,
    pub palette_open: bool,
    pub exit_armed: bool,
}

/// PURE: the whole keymap, as a function. `page` is [`crate::TuiConfig::page_lines`].
///
/// WP-1 owns the body.
pub fn action_for(key: KeyEvent, cx: KeyContext, page: u16) -> Action {
    let _ = (key, cx, page);
    todo!("WP-1: the keymap table of phase ux1 §2.1")
}

/// PURE: does this key take the keyboard back to the composer? A printable character with no
/// CONTROL and no ALT — and nothing else (B1: "any printable key snaps focus back").
pub fn snaps_to_composer(key: &KeyEvent) -> bool {
    let _ = key;
    todo!("WP-1: printable, no CONTROL, no ALT")
}

/// The key hints `/help` and the status line are BOTH generated from, so they cannot disagree
/// with each other or with [`action_for`] (M16).
pub fn hints() -> Vec<(&'static str, &'static str)> {
    todo!("WP-1: the fixed binding table of phase ux1 §2.1")
}

/// The two-press exit (B7).
pub struct ExitArm {
    armed_at: Option<DateTime<Utc>>,
    window: Duration,
}

/// What one `Ctrl+C` press means.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExitStep {
    /// First press: show `press Ctrl+C again to exit`.
    Arm,
    /// Second press inside the window: leave.
    Exit,
}

impl ExitArm {
    pub fn new(window: Duration) -> ExitArm {
        ExitArm {
            armed_at: None,
            window,
        }
    }

    /// PURE in `now`: first press arms and returns [`ExitStep::Arm`]; a second inside the window
    /// returns [`ExitStep::Exit`]; a press after the window re-arms.
    pub fn press(&mut self, now: DateTime<Utc>) -> ExitStep {
        let _ = (now, &self.armed_at, &self.window);
        todo!("WP-1")
    }

    pub fn is_armed(&self, now: DateTime<Utc>) -> bool {
        let _ = now;
        todo!("WP-1")
    }

    pub fn disarm(&mut self) {
        self.armed_at = None;
    }
}
