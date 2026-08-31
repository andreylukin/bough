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
    Scroll {
        delta: i16,
    },
    /// `End`: back to the tail.
    JumpLatest,
    /// Tab / BackTab: one step around the focus ring.
    CycleFocus(i32),
    /// `?` on an empty draft: the help the status line and `hints()` both advertise (M16).
    Help,
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
/// Order matters and is spelled once, here:
/// 1. the control chords, which mean the same thing everywhere;
/// 2. `Esc`, whose meaning depends on `running` then on an open overlay — and is NEVER draft
///    destruction (B3/M14);
/// 3. the paging keys, which drive the TRANSCRIPT from every context (B2) with exactly one
///    exception: `Home`/`End` in a non-empty composer draft move the caret;
/// 4. everything else falls through to whatever holds the keyboard.
pub fn action_for(key: KeyEvent, cx: KeyContext, page: u16) -> Action {
    use crossterm::event::{KeyCode, KeyModifiers};
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let page = page.clamp(1, i16::MAX as u16) as i16;

    match key.code {
        KeyCode::Char('c') if ctrl => return Action::ExitStep,
        KeyCode::Char('l') if ctrl => return Action::Redraw,
        KeyCode::Char('f') if ctrl => return Action::FocusSearch,
        // Tab belongs to the PALETTE while it is open — that is what completes the selected
        // command (§2.8). Cycling panes out from under an open palette is what made
        // `PaletteAction::Complete` unreachable.
        KeyCode::Tab if !cx.palette_open => return Action::CycleFocus(1),
        KeyCode::BackTab if !cx.palette_open => return Action::CycleFocus(-1),
        // `?` is advertised by `hints()` AND by the status line, so it has to mean something.
        // Only on an EMPTY draft: a `?` inside a sentence is a question mark.
        KeyCode::Char('?')
            if cx.focus_is_composer && cx.draft_is_empty && !cx.palette_open && !ctrl =>
        {
            return Action::Help
        }
        KeyCode::Esc => {
            // The running turn wins: `esc to interrupt` is the only stop key the status line
            // names, and it must mean that whatever else is on screen.
            if cx.running {
                return Action::Interrupt;
            }
            // With nothing to dismiss this is a deliberate NO-OP rather than `Pass`: passing Esc
            // to the composer is how the draft used to be destroyed (B3).
            return Action::DismissOverlay;
        }
        KeyCode::PageUp => return Action::Scroll { delta: -page },
        KeyCode::PageDown => return Action::Scroll { delta: page },
        KeyCode::Home | KeyCode::End => {
            // The ONE exception to "the paging keys drive the transcript from everywhere": a
            // draft with text in it owns its own line ends.
            if cx.focus_is_composer && !cx.draft_is_empty {
                return Action::Pass;
            }
            return match key.code {
                KeyCode::Home => Action::Scroll { delta: i16::MIN },
                _ => Action::JumpLatest,
            };
        }
        _ => {}
    }
    Action::Pass
}

/// PURE: does this key take the keyboard back to the composer? A printable character with no
/// CONTROL and no ALT — and nothing else (B1: "any printable key snaps focus back").
pub fn snaps_to_composer(key: &KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    match key.code {
        KeyCode::Char(_) => {
            !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
        }
        _ => false,
    }
}

/// The key hints `/help` and the status line are BOTH generated from, so they cannot disagree
/// with each other or with [`action_for`] (M16).
pub fn hints() -> Vec<(&'static str, &'static str)> {
    vec![
        ("enter", "send the message, or run a / command"),
        ("shift+enter", "start a new line in the message"),
        ("esc", "interrupt the running turn, or close an overlay"),
        ("ctrl+u", "clear the line"),
        ("up/down", "recall a sent message, or move the row focus"),
        ("enter/space", "open or close the focused tool row"),
        ("pgup/pgdn", "scroll the conversation"),
        ("home/end", "jump to the start, or to the latest"),
        ("tab", "move the keyboard to the next pane"),
        ("ctrl+f", "search the conversation"),
        ("ctrl+c", "interrupt, or press twice to exit"),
        ("ctrl+l", "redraw the screen"),
        ("?", "this help, on an empty message"),
    ]
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
        if self.is_armed(now) {
            self.armed_at = None;
            return ExitStep::Exit;
        }
        self.armed_at = Some(now);
        ExitStep::Arm
    }

    pub fn is_armed(&self, now: DateTime<Utc>) -> bool {
        match self.armed_at {
            // A clock that went backwards must not count as "inside the window": `to_std` fails
            // on a negative delta, and the honest answer there is "not armed".
            Some(at) => match (now - at).to_std() {
                Ok(elapsed) => elapsed <= self.window,
                Err(_) => false,
            },
            None => false,
        }
    }

    pub fn disarm(&mut self) {
        self.armed_at = None;
    }
}
