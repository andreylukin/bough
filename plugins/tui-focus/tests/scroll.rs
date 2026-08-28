//! WP-4 / §2.4 + V3: the scroll state machine, and the stability rule that is the whole reason it
//! is a state machine rather than an integer.

use bough_plugin_tui_focus::Scroll;

/// V3, stated as a unit: an ANCHORED viewport does not move when new steps arrive. This is what
/// makes it possible to read something while the agent is still streaming underneath.
#[test]
fn new_steps_do_not_move_an_anchored_viewport() {
    let anchored = Scroll::Anchored { top: 12 };
    assert_eq!(anchored.on_rows_appended(1), anchored);
    assert_eq!(anchored.on_rows_appended(500), anchored);
    // And the first row on screen is the same row before and after, which is the assertion the
    // shell-use script makes against the actual terminal.
    assert_eq!(anchored.top(100, 20), 12);
    assert_eq!(anchored.top(600, 20), 12);

    // `Follow`, by contrast, is defined to move: it is pinned to the bottom, wherever that is.
    assert_eq!(Scroll::Follow.on_rows_appended(3), Scroll::Follow);
    assert_eq!(Scroll::Follow.top(100, 20), 80);
    assert_eq!(Scroll::Follow.top(103, 20), 83);
}

/// Scrolling back down to the bottom RE-ARMS following. Without this an anchored-at-the-bottom
/// viewport would sit frozen while new text arrived just below it — the opposite of what anchoring
/// is for. `End` needs no special case: it is a large positive delta.
#[test]
fn follow_re_arms_at_the_bottom() {
    let rows = 50;
    let height = 10;
    // Scroll up: anchored.
    let up = Scroll::Follow.scrolled(-5, rows, height);
    assert_eq!(up, Scroll::Anchored { top: 35 });
    assert!(!up.is_following());
    // Back down by the same amount: at the bottom, and following again.
    assert_eq!(up.scrolled(5, rows, height), Scroll::Follow);
    // `End`.
    assert_eq!(up.scrolled(i32::MAX / 2, rows, height), Scroll::Follow);
    // Everything fits on screen, so there is nowhere to scroll and following is the only state.
    assert_eq!(Scroll::Follow.scrolled(-3, 4, 40), Scroll::Follow);
}

/// The clamps, both ends. A page key past the end must never produce a top beyond the last row or
/// a negative one — `top` is used as an index and a `scroll()` offset.
#[test]
fn page_down_past_the_end_clamps() {
    let rows = 50;
    let height = 10;
    let anchored = Scroll::Anchored { top: 20 };

    assert_eq!(anchored.scrolled(1_000_000, rows, height), Scroll::Follow);
    assert_eq!(
        anchored.scrolled(i32::MAX / 2, rows, height),
        Scroll::Follow
    );
    assert_eq!(
        anchored.scrolled(-1_000_000, rows, height),
        Scroll::Anchored { top: 0 }
    );
    assert_eq!(
        anchored.scrolled(i32::MIN / 2, rows, height),
        Scroll::Anchored { top: 0 }
    );

    // An anchor left behind by rows that paged out is clamped at READ time rather than corrupting
    // the state: `top` never exceeds the last possible top.
    assert_eq!(Scroll::Anchored { top: 999 }.top(rows, height), 40);
    // Zero rows and a zero-height pane are both survivable.
    assert_eq!(Scroll::Follow.top(0, 10), 0);
    assert_eq!(Scroll::Follow.scrolled(5, 0, 0), Scroll::Follow);
}

/// The scroll maths clamps against RENDERED LINES, not against rows.
///
/// `render` scrolls a `Paragraph` by a line index, and one row wraps to many lines — an expanded
/// tool call to dozens. Clamping against `rows.len()` made `max_top` zero for any trajectory whose
/// handful of steps still filled the screen, so every wheel and key scroll silently re-armed
/// `Follow` and the viewport never moved (V3, `the_wheel_scrolls_the_trajectory`).
#[test]
fn a_page_up_scrolls_when_few_rows_rendered_many_lines() {
    use bough_plugin_tui_focus::{FocusConfig, FocusPane, FocusState};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use parking_lot::Mutex;
    use std::sync::Arc;

    let cfg = Arc::new(FocusConfig {
        max_rows: 500,
        max_tool_lines: 40,
        page_lines: 10,
        expand_new_tools: false,
        show_reasoning: true,
        context: true,
        context_refresh_ms: 150,
    });
    let pane = FocusPane::new(
        cfg,
        Arc::new(Mutex::new(FocusState::default())),
        Arc::new(Mutex::new(Default::default())),
    );

    // Three steps' worth of rows, but the last frame rendered 80 lines into a 20-row viewport.
    let mut state = FocusState {
        height: 20,
        lines: 80,
        ..Default::default()
    };
    let page_up = KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE);
    assert_eq!(
        pane.scroll_for_key(page_up, &state),
        Some(Scroll::Anchored { top: 50 }),
        "one page up from the bottom of 80 lines"
    );

    // The same state measured in ROWS has nowhere to go, which is the bug this pins.
    state.lines = 3;
    assert_eq!(
        pane.scroll_for_key(page_up, &state),
        Some(Scroll::Follow),
        "three lines in a twenty-row viewport genuinely cannot scroll"
    );
}

// ---------------------------------------------------------------------------
// WP-3 / phase ux1 §2.2: `Viewport` — follow, count, badge, re-arm (B2)
// ---------------------------------------------------------------------------

use bough_plugin_tui_focus::Viewport;

/// The default is FOLLOWING, and following counts nothing: a viewport pinned to the tail has
/// already shown everything that arrived, so a `↓ N new` badge there would be a lie.
#[test]
fn a_following_viewport_counts_nothing_and_badges_nothing() {
    let mut v = Viewport::default();
    assert!(v.is_following());
    v.on_rows_appended(7);
    v.on_rows_appended(90);
    assert_eq!(v.unseen, 0);
    assert_eq!(v.badge(), None);
    // And it stays pinned to the bottom, wherever the bottom now is.
    assert_eq!(v.top(100, 20), 80);
}

/// Scrolled up, the viewport is ANCHORED: it does not move, and everything appended under it is
/// counted, so the affordance can say how much is waiting.
#[test]
fn an_anchored_viewport_counts_what_arrives_and_badges_it() {
    let mut v = Viewport::default();
    v.scrolled(-10, 100, 20);
    assert!(!v.is_following());
    let top = v.top(100, 20);

    v.on_rows_appended(1);
    assert_eq!(v.badge().as_deref(), Some("↓ 1 new"));
    v.on_rows_appended(2);
    assert_eq!(v.badge().as_deref(), Some("↓ 3 new"));
    // V3: the rows arriving underneath did NOT move the reader.
    assert_eq!(v.top(103, 20), top);
}

/// The re-arm, by every route the keymap offers: `End`, a scroll back to the bottom, and sending
/// a message. All three land at the tail with nothing outstanding.
#[test]
fn the_badge_clears_on_every_route_back_to_the_tail() {
    let anchored = || {
        let mut v = Viewport::default();
        v.scrolled(-10, 100, 20);
        v.on_rows_appended(5);
        assert_eq!(v.badge().as_deref(), Some("↓ 5 new"));
        v
    };

    // `End` (and sending a message, which calls the same thing).
    let mut v = anchored();
    v.to_latest();
    assert!(v.is_following());
    assert_eq!(v.badge(), None);

    // Scrolling back down by hand — all the way, including the five rows that arrived while
    // the reader was up here.
    let mut v = anchored();
    v.scrolled(15, 105, 20);
    assert!(v.is_following(), "landing at the bottom re-arms follow");
    assert_eq!(v.badge(), None);

    // A partial scroll back is still anchored, and still owes the reader the count.
    let mut v = anchored();
    v.scrolled(3, 105, 20);
    assert!(!v.is_following());
    assert_eq!(v.badge().as_deref(), Some("↓ 5 new"));

    // And once re-armed it counts again from zero the next time it detaches.
    let mut v = anchored();
    v.to_latest();
    v.scrolled(-4, 105, 20);
    v.on_rows_appended(2);
    assert_eq!(v.badge().as_deref(), Some("↓ 2 new"));
}

/// A jump to a search hit anchors WITHOUT pretending the tail has been read: the badge is what
/// tells the reader there is somewhere to come back to.
#[test]
fn anchoring_on_a_hit_keeps_the_outstanding_count() {
    let mut v = Viewport::default();
    v.scrolled(-10, 100, 20);
    v.on_rows_appended(4);
    v.anchor_on(12);
    assert_eq!(v.top(104, 20), 12);
    assert_eq!(v.badge().as_deref(), Some("↓ 4 new"));
    v.to_latest();
    assert_eq!(v.badge(), None);
}

/// Nothing to scroll is following: a short transcript can never show a badge, because there is
/// no way for the reader to be anywhere but at the end of it.
#[test]
fn a_transcript_that_fits_on_screen_never_badges() {
    let mut v = Viewport::default();
    v.scrolled(-30, 4, 40);
    v.on_rows_appended(1);
    assert!(v.is_following());
    assert_eq!(v.badge(), None);
}

/// Round 8: scrolled up with nothing new, the badge still says where the newest is.
#[test]
fn scrolled_up_with_nothing_new_badges_older() {
    let mut v = Viewport::default();
    v.scrolled(-5, 100, 20);
    assert!(!v.is_following());
    assert_eq!(v.badge().as_deref(), Some("↑ older"));
    v.on_rows_appended(2);
    assert_eq!(v.badge().as_deref(), Some("↓ 2 new"));
}
