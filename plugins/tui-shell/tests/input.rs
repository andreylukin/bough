//! §11's input contract: Enter means exactly one of two things and the two paths cannot cross
//! (V5); `tui/key` is the extension point a plugin gets instead of editing the keymap (P3-D18);
//! and the mouse routes by geometry — a click focuses, a wheel does not.

mod common;

use bough_kernel::Next;
use bough_plugin_tui_shell::events::{KeyDispatch, TuiKeyEvent};
use bough_plugin_tui_shell::{run, PaneId, Slot, SlotSize};
use common::{add_pane, focused_agent, shell, shell_with_agents};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

async fn typed(tui: &bough_plugin_tui_shell::TuiHandle, text: &str) {
    for c in text.chars() {
        run::on_key(tui, key(KeyCode::Char(c))).await;
    }
}

#[tokio::test]
async fn enter_on_plain_text_sends_a_followup_to_the_focused_agent() {
    let (_ctx, tui, agents, factory) = shell_with_agents().await;
    let agent = focused_agent(&tui, &agents, "sol").await;

    typed(&tui, "rebase the loop").await;
    run::on_key(&tui, key(KeyCode::Enter)).await;

    // The message is in the agent's inbox, durably spliced, and the driver was told.
    assert_eq!(agent.inbox().len(), 1, "the followup reached the inbox");
    assert_eq!(factory.last().notifies(), 1, "the driver was notified once");
    assert_eq!(tui.composer_text(), "", "the composer cleared");
    assert_eq!(tui.last_command(), None, "nothing went to `ctx.commands`");
}

#[tokio::test]
async fn enter_on_a_slash_line_dispatches_a_command_and_never_sends() {
    let (ctx, tui, agents, factory) = shell_with_agents().await;
    let agent = focused_agent(&tui, &agents, "sol").await;
    // The four built-ins, registered the way `apply` registers them.
    common::register_builtins(&ctx, &tui).await;

    typed(&tui, "/help").await;
    run::on_key(&tui, key(KeyCode::Enter)).await;

    assert_eq!(
        tui.last_command(),
        Some("/help".to_string()),
        "the line went to the command path"
    );
    assert_eq!(agent.inbox().len(), 0, "and NOT to the agent");
    assert_eq!(factory.last().notifies(), 0, "the driver heard nothing");
    // `/help` ran, and its output — not a message — is what the surface shows.
    let notice = tui.notice().unwrap_or_default();
    assert!(notice.contains("/quit"), "the help text: {notice:?}");
    // The key list is GENERATED from `keymap::hints`, in plain language (M16): the help and the
    // keymap cannot disagree because there is only one table.
    assert!(notice.contains("tab"), "the keymap: {notice:?}");
    assert!(
        notice.contains("move the keyboard to the next pane"),
        "the keymap, in plain language: {notice:?}"
    );
}

#[tokio::test]
async fn alt_enter_inserts_a_newline() {
    let (_ctx, tui) = shell();
    typed(&tui, "one").await;
    run::on_key(&tui, KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)).await;
    typed(&tui, "two").await;

    assert_eq!(tui.composer_text(), "one\ntwo");
    assert_eq!(tui.last_command(), None, "nothing was dispatched");
}

#[tokio::test]
async fn a_tui_key_listener_that_sets_handled_consumes_the_key() {
    let (ctx, tui) = shell();
    let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<char>::new()));
    let recorder = seen.clone();

    ctx.on_waterfall::<TuiKeyEvent, _, _>(move |mut kd: KeyDispatch, next: Next<TuiKeyEvent>| {
        let recorder = recorder.clone();
        async move {
            if kd.key.code == KeyCode::Char('x') && kd.key.kind != KeyEventKind::Release {
                recorder.lock().push('x');
                kd.handled = true;
                // A listener that consumes still DELEGATES: the rule is `next()`, always.
                return next.run(kd).await;
            }
            next.run(kd).await
        }
    })
    .await
    .expect("the listener registers");

    typed(&tui, "ax").await;

    assert_eq!(&*seen.lock(), &['x']);
    assert_eq!(
        tui.composer_text(),
        "a",
        "the consumed key never reached the composer"
    );
}

#[tokio::test]
async fn a_click_focuses_the_pane_under_the_pointer_and_forwards_its_hit() {
    let (ctx, tui) = shell();
    let (rail, _) = add_pane(&ctx, &tui, "rail", Slot::Strip, 0, SlotSize::Cells(20)).await;
    let (focus, _) = add_pane(&ctx, &tui, "focus", Slot::Main, 0, SlotSize::Fill(1)).await;
    run::draw(&tui);

    // Row 0 of the rail is the region the recorder records as a hit.
    run::on_mouse(
        &tui,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
    )
    .await;

    // phase ux1 B1: a click ACTS on the row it landed on and does NOT move the keyboard. The
    // composer keeps it, so the next thing typed still goes into the message being written.
    assert!(
        tui.composer_focused(),
        "the click did not steal the keyboard"
    );
    let events = rail.events();
    assert!(
        events
            .iter()
            .any(|e| e.starts_with("click:(3, 0):row:rail")),
        "the pane got the click AND the hit it had recorded: {events:?}"
    );
    assert!(
        focus.events().iter().all(|e| !e.starts_with("click")),
        "the pane under nobody's pointer heard nothing"
    );
}

#[tokio::test]
async fn a_wheel_event_scrolls_the_pane_under_the_pointer_without_moving_focus() {
    let (ctx, tui) = shell();
    let (rail, _) = add_pane(&ctx, &tui, "rail", Slot::Strip, 0, SlotSize::Cells(20)).await;
    let (focus, _) = add_pane(&ctx, &tui, "focus", Slot::Main, 0, SlotSize::Fill(1)).await;
    run::draw(&tui);
    tui.focus_pane(PaneId::new("rail")).await;

    run::on_mouse(
        &tui,
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 40, // over `focus`, which does NOT have keyboard focus
            row: 5,
            modifiers: KeyModifiers::NONE,
        },
    )
    .await;

    assert_eq!(focus.scrolled(), 3, "the pane under the pointer scrolled");
    assert_eq!(rail.scrolled(), 0, "the focused pane did not");
    assert_eq!(
        tui.focused_pane(),
        PaneId::new("rail"),
        "and focus did not move"
    );
}

/// PageUp/PageDown reach the focused pane even though the COMPOSER holds the keyboard.
///
/// The composer holds focus for the whole session, so a rule that let it swallow every key it
/// does not use meant the trajectory could not be paged from the keyboard at all (V3). Up/Down are
/// the counter-case: those are the composer's own cursor and must NOT leak.
#[tokio::test]
async fn page_keys_reach_the_focused_pane_while_the_composer_holds_the_keyboard() {
    let (ctx, tui) = shell();
    let (pane, _h) = add_pane(&ctx, &tui, "focus", Slot::Main, 0, SlotSize::Fill(1)).await;
    assert!(
        tui.composer_focused(),
        "the composer starts with the keyboard"
    );

    run::on_key(&tui, key(KeyCode::PageUp)).await;
    run::on_key(&tui, key(KeyCode::PageDown)).await;
    // phase ux1 B2: the paging keys are a SCROLL of the transcript from every context. They are
    // no longer offered to the pane as a key first — one meaning, decided once, in `action_for`.
    assert_eq!(
        pane.events(),
        vec!["scroll:-10", "scroll:10"],
        "the paging keys scrolled the transcript"
    );
    assert!(
        tui.composer_focused(),
        "and the composer still has the keyboard"
    );

    run::on_key(&tui, key(KeyCode::Up)).await;
    assert_eq!(
        pane.events().len(),
        2,
        "Up is the composer's cursor and never reaches the pane"
    );
    assert_eq!(pane.scrolled(), 0, "the two page scrolls cancelled out");
}
