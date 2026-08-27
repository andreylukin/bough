//! The keymap as a TRUTH TABLE (phase ux1 §2.1). Every row here is a key whose meaning the audit
//! found wrong, and the point of the table is that the meaning depends on the `KeyContext` in
//! exactly the ways spelled in the plan and in no others.

use bough_plugin_tui_shell::{
    action_for, snaps_to_composer, Action, ExitArm, ExitStep, KeyContext,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;

const PAGE: u16 = 10;

fn k(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// Every combination of the six flags that can differ, as a list to sweep.
fn every_context() -> Vec<KeyContext> {
    let mut out = Vec::new();
    for focus_is_composer in [true, false] {
        for draft_is_empty in [true, false] {
            for running in [true, false] {
                for overlay_open in [true, false] {
                    for palette_open in [true, false] {
                        for exit_armed in [true, false] {
                            out.push(KeyContext {
                                focus_is_composer,
                                draft_is_empty,
                                running,
                                overlay_open,
                                palette_open,
                                exit_armed,
                            });
                        }
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// B2: the paging keys mean the same thing from EVERY context
// ---------------------------------------------------------------------------

#[test]
fn pageup_and_pagedown_scroll_the_transcript_from_every_context() {
    for cx in every_context() {
        assert_eq!(
            action_for(k(KeyCode::PageUp), cx, PAGE),
            Action::Scroll { delta: -10 },
            "PageUp must page the transcript whatever else is true: {cx:?}"
        );
        assert_eq!(
            action_for(k(KeyCode::PageDown), cx, PAGE),
            Action::Scroll { delta: 10 },
            "{cx:?}"
        );
    }
}

#[test]
fn home_and_end_scroll_everywhere_except_over_a_non_empty_draft() {
    for cx in every_context() {
        let caret_case = cx.focus_is_composer && !cx.draft_is_empty;
        let home = action_for(k(KeyCode::Home), cx, PAGE);
        let end = action_for(k(KeyCode::End), cx, PAGE);
        if caret_case {
            // The ONE exception in the whole table: a draft with text owns its own line ends.
            assert_eq!(home, Action::Pass, "{cx:?}");
            assert_eq!(end, Action::Pass, "{cx:?}");
        } else {
            assert_eq!(home, Action::Scroll { delta: i16::MIN }, "{cx:?}");
            assert_eq!(end, Action::JumpLatest, "{cx:?}");
        }
    }
}

// ---------------------------------------------------------------------------
// B3/B7/M14: what Esc means, and what it never means
// ---------------------------------------------------------------------------

#[test]
fn esc_interrupts_while_running_and_otherwise_dismisses_and_never_reaches_the_draft() {
    for cx in every_context() {
        let action = action_for(k(KeyCode::Esc), cx, PAGE);
        if cx.running {
            assert_eq!(action, Action::Interrupt, "{cx:?}");
        } else {
            assert_eq!(action, Action::DismissOverlay, "{cx:?}");
        }
        assert_ne!(
            action,
            Action::Pass,
            "Esc must never fall through to the composer: that is how a draft got destroyed ({cx:?})"
        );
    }
}

#[test]
fn the_chords_mean_one_thing_each_from_every_context() {
    for cx in every_context() {
        assert_eq!(action_for(ctrl('c'), cx, PAGE), Action::ExitStep, "{cx:?}");
        assert_eq!(action_for(ctrl('l'), cx, PAGE), Action::Redraw, "{cx:?}");
        assert_eq!(
            action_for(ctrl('f'), cx, PAGE),
            Action::FocusSearch,
            "{cx:?}"
        );
        // Tab/BackTab are the ONE pair whose meaning is context-dependent, and deliberately so:
        // while the palette is open they belong to it (that is what completes the selected
        // command), and cycling panes out from under it is what made `PaletteAction::Complete`
        // unreachable in the shipped binary. Everywhere else they cycle pane focus.
        let (tab, backtab) = (
            action_for(k(KeyCode::Tab), cx, PAGE),
            action_for(k(KeyCode::BackTab), cx, PAGE),
        );
        if cx.palette_open {
            assert_eq!(
                tab,
                Action::Pass,
                "Tab must reach the OPEN palette, not the pane ring ({cx:?})"
            );
            assert_eq!(
                backtab,
                Action::Pass,
                "BackTab must reach the OPEN palette, not the pane ring ({cx:?})"
            );
        } else {
            assert_eq!(tab, Action::CycleFocus(1), "{cx:?}");
            assert_eq!(backtab, Action::CycleFocus(-1), "{cx:?}");
        }
    }
}

#[test]
fn an_ordinary_key_is_never_the_shells() {
    for cx in every_context() {
        for code in [
            KeyCode::Char('a'),
            KeyCode::Enter,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Backspace,
        ] {
            assert_eq!(
                action_for(k(code), cx, PAGE),
                Action::Pass,
                "{code:?} belongs to whatever has the keyboard: {cx:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// B1: any printable key takes the keyboard back
// ---------------------------------------------------------------------------

#[test]
fn snaps_to_composer_accepts_a_printable_character_and_nothing_else() {
    assert!(snaps_to_composer(&k(KeyCode::Char('a'))));
    assert!(snaps_to_composer(&KeyEvent::new(
        KeyCode::Char('A'),
        KeyModifiers::SHIFT
    )));
    assert!(snaps_to_composer(&k(KeyCode::Char(' '))));

    assert!(!snaps_to_composer(&ctrl('a')), "Ctrl+a is a chord");
    assert!(
        !snaps_to_composer(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT)),
        "Alt+a is a chord"
    );
    assert!(!snaps_to_composer(&k(KeyCode::F(1))));
    assert!(!snaps_to_composer(&k(KeyCode::Enter)));
    assert!(!snaps_to_composer(&k(KeyCode::Up)));
    assert!(!snaps_to_composer(&k(KeyCode::Esc)));
}

// ---------------------------------------------------------------------------
// B7: the two-press exit
// ---------------------------------------------------------------------------

fn t(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(1_700_000_000_000).unwrap() + ChronoDuration::milliseconds(ms)
}

#[test]
fn exit_arms_then_exits_inside_the_window_and_re_arms_after_it() {
    let mut arm = ExitArm::new(Duration::from_millis(3000));

    assert_eq!(arm.press(t(0)), ExitStep::Arm, "the first press only arms");
    assert!(arm.is_armed(t(10)));
    assert_eq!(
        arm.press(t(500)),
        ExitStep::Exit,
        "a second press inside the window leaves"
    );
    assert!(!arm.is_armed(t(510)), "and leaves nothing armed behind it");

    // Past the window, a press ARMS again rather than exiting: a Ctrl+C the user forgot about
    // must not turn the next one into a quit.
    assert_eq!(arm.press(t(1_000)), ExitStep::Arm);
    assert!(!arm.is_armed(t(9_000)), "the window lapsed");
    assert_eq!(arm.press(t(9_000)), ExitStep::Arm, "so this one re-arms");
    assert_eq!(arm.press(t(9_100)), ExitStep::Exit);
}

#[test]
fn disarm_forgets_the_pending_press() {
    let mut arm = ExitArm::new(Duration::from_millis(3000));
    arm.press(t(0));
    arm.disarm();
    assert!(!arm.is_armed(t(1)));
    assert_eq!(arm.press(t(2)), ExitStep::Arm, "it starts over");
}

// ---------------------------------------------------------------------------
// M16: the help and the keymap are ONE table
// ---------------------------------------------------------------------------

#[test]
fn every_hint_is_plain_language_and_names_a_key() {
    let hints = bough_plugin_tui_shell::hints();
    assert!(!hints.is_empty());
    for (key, meaning) in &hints {
        assert!(!key.is_empty(), "a hint with no key: {meaning}");
        assert!(
            meaning.chars().next().is_some_and(|c| c.is_lowercase()),
            "a hint reads as a sentence: {meaning:?}"
        );
        assert_eq!(
            bough_plugin_commands::palette::house_word(meaning),
            None,
            "the internal vocabulary must not reach a key hint: {meaning:?}"
        );
    }
    let keys: Vec<&str> = hints.iter().map(|(k, _)| *k).collect();
    for want in ["esc", "ctrl+c", "pgup/pgdn", "tab", "ctrl+f"] {
        assert!(
            keys.contains(&want),
            "the table is missing {want}: {keys:?}"
        );
    }
}
