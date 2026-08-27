//! The focus model (phase ux1 §2.1, B1/B2/B6/B7). One always-live composer; the keyboard moves
//! only when the user says so; a click reads and never redirects typing; the paging keys drive
//! the transcript from wherever the keyboard is.

mod common;

use bough_plugin_tui_shell::{run, NoticeKind, PaneId, Slot, SlotSize};
use common::{add_pane, shell};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn click(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[tokio::test]
async fn a_click_on_a_pane_acts_on_it_and_leaves_the_keyboard_where_it_was() {
    let (ctx, tui) = shell();
    let (rail, _) = add_pane(&ctx, &tui, "rail", Slot::Strip, 0, SlotSize::Cells(20)).await;
    let (_focus, _) = add_pane(&ctx, &tui, "focus", Slot::Main, 0, SlotSize::Fill(1)).await;
    run::draw(&tui);

    run::on_mouse(&tui, click(3, 0)).await;

    assert!(
        tui.composer_focused(),
        "B1: the click did not steal the keyboard from the composer"
    );
    assert!(
        rail.events().iter().any(|e| e.starts_with("click:")),
        "and the pane still ACTED on the click: {:?}",
        rail.events()
    );

    // The proof that matters: what is typed next is still a message.
    run::on_key(&tui, key(KeyCode::Char('h'))).await;
    run::on_key(&tui, key(KeyCode::Char('i'))).await;
    assert_eq!(
        tui.composer_text(),
        "hi",
        "the typing went where the user was looking"
    );
}

#[tokio::test]
async fn tab_moves_the_keyboard_deliberately_and_a_printable_key_brings_it_back() {
    let (ctx, tui) = shell();
    let (_rail, _) = add_pane(&ctx, &tui, "rail", Slot::Strip, 0, SlotSize::Cells(20)).await;
    let (_focus, _) = add_pane(&ctx, &tui, "focus", Slot::Main, 0, SlotSize::Fill(1)).await;
    run::draw(&tui);
    assert!(tui.composer_focused());

    run::on_key(&tui, key(KeyCode::Tab)).await;
    assert!(!tui.composer_focused(), "Tab gave the keyboard to a pane");
    let landed = tui.focused_pane();
    assert_ne!(landed, bough_plugin_tui_shell::no_pane());

    // Shift+Tab walks back the other way, so the ring is a ring and not a one-way street.
    run::on_key(&tui, key(KeyCode::BackTab)).await;
    assert!(
        tui.composer_focused(),
        "Shift+Tab came back to the composer"
    );

    // And once the keyboard is away, ONE printable character is enough to take it back (B1).
    run::on_key(&tui, key(KeyCode::Tab)).await;
    assert!(!tui.composer_focused());
    run::on_key(&tui, key(KeyCode::Char('x'))).await;
    assert!(tui.composer_focused(), "a printable key snapped it back");
    assert_eq!(tui.composer_text(), "x", "and the character was not eaten");
}

#[tokio::test]
async fn a_control_chord_does_not_snap_the_keyboard_back() {
    let (ctx, tui) = shell();
    let (_focus, _) = add_pane(&ctx, &tui, "focus", Slot::Main, 0, SlotSize::Fill(1)).await;
    run::draw(&tui);
    run::on_key(&tui, key(KeyCode::Tab)).await;
    assert!(!tui.composer_focused());

    run::on_key(
        &tui,
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
    )
    .await;
    assert!(
        !tui.composer_focused(),
        "a chord the pane reads leaves the keyboard where the user put it"
    );
}

#[tokio::test]
async fn the_paging_keys_drive_the_transcript_from_either_side_of_the_ring() {
    let (ctx, tui) = shell();
    let (rail, _) = add_pane(&ctx, &tui, "rail", Slot::Strip, 0, SlotSize::Cells(20)).await;
    let (transcript, _) = add_pane(&ctx, &tui, "tui.focus", Slot::Main, 0, SlotSize::Fill(1)).await;
    run::draw(&tui);

    // With the COMPOSER holding the keyboard.
    run::on_key(&tui, key(KeyCode::PageUp)).await;
    assert_eq!(transcript.scrolled(), -10);
    assert_eq!(rail.scrolled(), 0, "the rail is not the transcript");

    // And with the RAIL holding it: the same key, the same pane (B2).
    tui.focus_pane(PaneId::new("rail")).await;
    run::on_key(&tui, key(KeyCode::PageDown)).await;
    assert_eq!(transcript.scrolled(), 0, "-10 then +10");
    assert_eq!(rail.scrolled(), 0, "the focused pane was NOT scrolled");
    assert_eq!(
        tui.focused_pane(),
        PaneId::new("rail"),
        "and paging did not move the keyboard"
    );
}

#[tokio::test]
async fn the_wheel_over_the_composer_still_scrolls_the_conversation() {
    let (ctx, tui) = shell();
    let (transcript, _) = add_pane(&ctx, &tui, "tui.focus", Slot::Main, 0, SlotSize::Fill(1)).await;
    run::draw(&tui);

    let size = tui.size();
    run::on_mouse(
        &tui,
        MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 4,
            row: size.height - 1, // the composer band: no pane lives here
            modifiers: KeyModifiers::NONE,
        },
    )
    .await;
    assert_eq!(
        transcript.scrolled(),
        -3,
        "M23: a wheel over the composer means the only thing it can mean"
    );
    assert!(tui.composer_focused(), "and focus did not move");
}

#[tokio::test]
async fn esc_with_nothing_up_leaves_the_draft_alone() {
    let (_ctx, tui) = shell();
    tui.set_composer_text("half a thought");
    run::on_key(&tui, key(KeyCode::Esc)).await;
    assert_eq!(
        tui.composer_text(),
        "half a thought",
        "V3: Esc destroys nothing"
    );
}

#[tokio::test]
async fn esc_dismisses_a_notice_before_it_reaches_anything_else() {
    let (_ctx, tui) = shell();
    tui.notify_kind("something went wrong", NoticeKind::Error);
    tui.set_composer_text("still here");
    run::on_key(&tui, key(KeyCode::Esc)).await;
    assert_eq!(tui.notice(), None, "the overlay went");
    assert_eq!(tui.composer_text(), "still here", "the draft did not");
}

#[tokio::test]
async fn ctrl_c_while_idle_arms_once_and_says_so() {
    let (_ctx, tui) = shell();
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

    run::on_key(&tui, ctrl_c).await;
    assert!(tui.exit_armed(), "B7: the first press only arms");
    let notice = tui.notice().unwrap_or_default();
    assert!(
        notice.contains("Ctrl+C again"),
        "and the screen says how to finish: {notice:?}"
    );

    // Any other key changes its mind.
    run::on_key(&tui, key(KeyCode::Char('a'))).await;
    assert!(!tui.exit_armed(), "a change of mind disarms it");
}

#[tokio::test]
async fn the_notice_carries_its_role_and_an_error_waits_for_a_key() {
    let (_ctx, tui) = shell();
    let now = chrono::Utc::now();

    tui.notify_kind("copied 12 chars", NoticeKind::Copied);
    let n = tui.notice_now(now).expect("a live notice");
    assert_eq!(n.kind, NoticeKind::Copied);
    assert!(n.ttl.is_some(), "a flash fades on its own");
    assert_eq!(
        tui.notice_now(now + chrono::Duration::seconds(30)),
        None,
        "and is gone by then"
    );

    tui.notify_kind("no search pane", NoticeKind::Error);
    let n = tui.notice_now(now).expect("a live notice");
    assert_eq!(n.kind, NoticeKind::Error);
    assert_eq!(n.ttl, None, "M22: an error waits to be read");
    assert!(
        tui.notice_now(now + chrono::Duration::hours(1)).is_some(),
        "however long that takes"
    );
}

#[tokio::test]
async fn the_transcript_pane_is_matched_exactly() {
    let (ctx, tui) = shell();
    let (_other, _) = add_pane(
        &ctx,
        &tui,
        "tui.focus.old",
        Slot::Main,
        0,
        SlotSize::Fill(1),
    )
    .await;
    assert_eq!(
        tui.transcript_pane(),
        None,
        "a substring match is how `search_pane` was broken once already"
    );
    let (_real, _) = add_pane(&ctx, &tui, "tui.focus", Slot::Main, 1, SlotSize::Fill(1)).await;
    assert_eq!(tui.transcript_pane(), Some(PaneId::new("tui.focus")));
}
