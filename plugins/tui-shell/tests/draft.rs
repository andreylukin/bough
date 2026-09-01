//! Phase ux1 §2.3, V3: **nothing the user typed is deleted by anything except an explicit clear.**
//!
//! These drive the draft the way `run::on_key` does — `PasteBurst::on_key(now)` first, its verdict
//! handed to `Composer::on_key(key, in_burst)` — so the sequencing rule the plan states once is
//! pinned here rather than assumed. The shell's own wiring of the same pieces is WP-1's; what this
//! file proves is that the pieces answer correctly for every walk the audit recorded.

use std::time::Duration;

use bough_plugin_tui_shell::draft::{kill_to_line_start, PasteBurst, SentHistory};
use bough_plugin_tui_shell::{test_config, Composer, ComposerAction};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn composer() -> Composer {
    Composer::new(&test_config())
}

fn at(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(1_700_000_000_000 + ms).unwrap()
}

/// One key through the pair, exactly as `run::on_key` sequences them.
fn feed(
    c: &mut Composer,
    burst: &mut PasteBurst,
    now: DateTime<Utc>,
    key: KeyEvent,
) -> ComposerAction {
    let in_burst = burst.on_key(now);
    c.on_key(key, in_burst)
}

fn ch(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
}

/// B4: a raw three-line paste into a terminal that does not advertise bracketed paste.
#[test]
fn a_three_line_paste_burst_becomes_one_draft_and_one_send() {
    let mut c = composer();
    let mut burst = PasteBurst::new(Duration::from_millis(20));
    let mut sends = Vec::new();
    let mut clock = 0;

    // The whole paste arrives in a millisecond and a half.
    for key in "one\ntwo\nthree"
        .chars()
        .map(|c| if c == '\n' { enter() } else { ch(c) })
    {
        if let ComposerAction::Send(text) = feed(&mut c, &mut burst, at(clock), key) {
            sends.push(text);
        }
        clock += 1;
    }

    assert!(sends.is_empty(), "the paste sent nothing on its own");
    assert_eq!(
        c.text(),
        "one\ntwo\nthree",
        "all three lines are one draft the user can still edit"
    );

    // The user's own Enter, a second later, sends the lot.
    let sent = feed(&mut c, &mut burst, at(clock + 1_000), enter());
    assert_eq!(sent, ComposerAction::Send("one\ntwo\nthree".to_string()));
    assert!(c.is_empty(), "and the send is the explicit clear");
}

/// The counter-case: the same three lines, typed by a human, are three messages.
#[test]
fn the_same_three_lines_typed_slowly_are_three_sends() {
    let mut c = composer();
    let mut burst = PasteBurst::new(Duration::from_millis(20));
    let mut sends = Vec::new();
    let mut clock = 0;

    for line in ["one", "two", "three"] {
        for k in line.chars() {
            // A fast typist: 60ms a key is still nowhere near a paste.
            clock += 60;
            feed(&mut c, &mut burst, at(clock), ch(k));
        }
        clock += 200;
        if let ComposerAction::Send(text) = feed(&mut c, &mut burst, at(clock), enter()) {
            sends.push(text);
        }
    }

    assert_eq!(sends, vec!["one", "two", "three"]);
}

/// B3: a slash line nobody claimed. The text stays; a second unchanged Enter sends it.
#[test]
fn a_missed_command_keeps_its_text_and_the_next_enter_sends_it_as_a_message() {
    let mut c = composer();
    let mut burst = PasteBurst::new(Duration::from_millis(20));
    let mut clock = 0;
    for k in "/summarise this".chars() {
        clock += 50;
        feed(&mut c, &mut burst, at(clock), ch(k));
    }
    clock += 300;
    assert_eq!(
        feed(&mut c, &mut burst, at(clock), enter()),
        ComposerAction::Command("/summarise this".to_string())
    );
    assert_eq!(
        c.text(),
        "/summarise this",
        "Enter on a command line no longer clears the buffer"
    );

    // The shell resolved nothing, so it arms the way out rather than eating the line.
    c.arm_send_as_message();
    clock += 900;
    assert_eq!(
        feed(&mut c, &mut burst, at(clock), enter()),
        ComposerAction::Send("/summarise this".to_string()),
        "the second unchanged Enter sends it as a message"
    );
}

/// And the deliberate escape: `//x` is a message that starts with a slash.
#[test]
fn a_doubled_prefix_sends_one_slash_as_a_message() {
    let mut c = composer();
    let mut burst = PasteBurst::new(Duration::from_millis(20));
    let mut clock = 0;
    for k in "//x".chars() {
        clock += 50;
        feed(&mut c, &mut burst, at(clock), ch(k));
    }
    clock += 300;
    assert_eq!(
        feed(&mut c, &mut burst, at(clock), enter()),
        ComposerAction::Send("/x".to_string())
    );
}

/// V3, the flat rule: Esc is not a delete.
#[test]
fn esc_on_a_non_empty_draft_leaves_it_alone() {
    let mut c = composer();
    let mut burst = PasteBurst::new(Duration::from_millis(20));
    let mut clock = 0;
    for k in "draft".chars() {
        clock += 50;
        feed(&mut c, &mut burst, at(clock), ch(k));
    }
    clock += 300;
    let action = feed(
        &mut c,
        &mut burst,
        at(clock),
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert_eq!(action, ComposerAction::None);
    assert_eq!(c.text(), "draft");
}

/// M20: Ctrl+U is readline's kill-to-line-start, not a one-character delete.
#[test]
fn ctrl_u_kills_to_the_start_of_the_line() {
    let mut c = composer();
    for k in "abcdefgh".chars() {
        c.on_key(ch(k), false);
    }
    assert_eq!(
        c.on_key(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            false
        ),
        ComposerAction::Cleared
    );
    assert_eq!(c.text(), "");

    for k in "abcdef".chars() {
        c.on_key(ch(k), false);
    }
    for _ in 0..3 {
        c.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), false);
    }
    c.on_key(
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        false,
    );
    assert_eq!(c.text(), "def");

    // And the pure function underneath says the same thing.
    assert_eq!(kill_to_line_start("abcdefgh", 8), (String::new(), 0));
    assert_eq!(kill_to_line_start("abcdef", 3), ("def".to_string(), 0));
}

/// M20: `↑`/`↓` over an empty draft recall what was sent, and never lose the live draft.
#[test]
fn history_round_trips_and_hands_the_live_draft_back() {
    let mut h = SentHistory::new(test_config().history_cap);
    h.push("first message");
    h.push("second message");

    assert_eq!(h.prev("half-written"), Some("second message".to_string()));
    assert_eq!(h.prev("half-written"), Some("first message".to_string()));
    assert_eq!(h.prev("half-written"), None, "the oldest does not wrap");
    assert_eq!(h.next(), Some("second message".to_string()));
    assert_eq!(
        h.next(),
        Some("half-written".to_string()),
        "the draft the user was in the middle of came back untouched"
    );
}

/// The composer's own keys do the same walk, and a non-empty draft keeps `↑` as its cursor.
#[test]
fn up_over_an_empty_draft_recalls_and_over_a_written_one_does_not() {
    let mut c = composer();
    for k in "hello there".chars() {
        c.on_key(ch(k), false);
    }
    c.on_key(enter(), false);

    for k in "in progress".chars() {
        c.on_key(ch(k), false);
    }
    c.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), false);
    assert_eq!(
        c.text(),
        "in progress",
        "a written draft is never replaced by history"
    );

    c.clear();
    c.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), false);
    assert_eq!(c.text(), "hello there");
    c.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), false);
    assert_eq!(c.text(), "", "and back down to where the user was");
}

/// Minor 33: clicking in the composer puts the caret under the pointer.
#[test]
fn a_click_moves_the_caret_rather_than_the_focus() {
    use ratatui::layout::Rect;
    let mut c = composer();
    for k in "hello".chars() {
        c.on_key(ch(k), false);
    }
    let area = Rect::new(4, 20, 40, 1);
    // Column 4 + the two-cell prompt glyph + two characters in.
    c.caret_at(4 + 2 + 2, 20, area);
    // Typing lands where the caret was put, not at the end.
    c.on_key(ch('X'), false);
    assert_eq!(c.text(), "heXllo");
}

/// M16: the placeholder is a sentence, and it names both things Enter can do.
#[test]
fn the_placeholder_is_a_sentence() {
    assert_eq!(
        Composer::placeholder(),
        "Type a message, or / for a command"
    );
}
