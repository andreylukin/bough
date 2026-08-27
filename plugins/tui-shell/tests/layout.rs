//! §11's slot layout: deterministic order, the ZERO-SPACE rule, and reflow on unload — which is
//! the shape the phase's SWAP gate is measured in. A pane row disabled by patch must reflow the
//! remaining panes with no restart, and that is `removing_a_pane_reflows_the_remaining_ones`.

mod common;

use bough_plugin_tui_shell::pane::layout;
use bough_plugin_tui_shell::{PaneId, Slot, SlotSize};
use common::{add_pane, shell};
use ratatui::layout::Rect;

#[tokio::test]
async fn panes_lay_out_by_slot_then_order_then_id() {
    let (ctx, tui) = shell();
    // Registered in a deliberately wrong order: the layout, not the registration, decides.
    add_pane(&ctx, &tui, "zzz", Slot::Main, 5, SlotSize::Fill(1)).await;
    add_pane(&ctx, &tui, "aux", Slot::Aux, 0, SlotSize::Cells(4)).await;
    add_pane(&ctx, &tui, "aaa", Slot::Main, 5, SlotSize::Fill(1)).await;
    add_pane(&ctx, &tui, "rail", Slot::Strip, 0, SlotSize::Cells(20)).await;
    add_pane(&ctx, &tui, "early", Slot::Main, -1, SlotSize::Fill(1)).await;

    let ids: Vec<String> = tui.panes().into_iter().map(|p| p.id.to_string()).collect();
    assert_eq!(
        ids,
        vec![
            "rail",  // Strip
            "early", // Main, order -1
            "aaa",   // Main, order 5, id "aaa" before "zzz"
            "zzz", "aux", // Aux
        ]
    );

    // And the geometry follows the same order: the rail is the left column, Aux is at the bottom.
    let rects = layout(Rect::new(0, 0, 80, 24), &tui.panes(), 1, 0);
    let of = |id: &str| rects.iter().find(|(p, _)| p.as_str() == id).unwrap().1;
    assert_eq!(of("rail").x, 0);
    assert_eq!(of("rail").width, 20);
    assert_eq!(of("early").x, 20);
    assert!(of("aux").y > of("early").y);
}

#[tokio::test]
async fn a_slot_with_no_panes_takes_no_space() {
    let (ctx, tui) = shell();
    // No Strip pane, no Aux pane, no Status pane: Main gets everything above the composer.
    add_pane(&ctx, &tui, "focus", Slot::Main, 0, SlotSize::Fill(1)).await;
    let size = Rect::new(0, 0, 80, 24);
    let rects = layout(size, &tui.panes(), 1, 0);
    assert_eq!(rects.len(), 1);
    assert_eq!(rects[0].1, Rect::new(0, 0, 80, 23));

    // Add the rail: now, and only now, the Strip slot costs columns.
    let (_, rail) = add_pane(&ctx, &tui, "rail", Slot::Strip, 0, SlotSize::Cells(20)).await;
    let rects = layout(size, &tui.panes(), 1, 0);
    let main = rects.iter().find(|(p, _)| p.as_str() == "focus").unwrap().1;
    assert_eq!(main, Rect::new(20, 0, 60, 23));
    rail.dispose().await;
}

#[tokio::test]
async fn removing_a_pane_reflows_the_remaining_ones() {
    let (ctx, tui) = shell();
    add_pane(&ctx, &tui, "focus", Slot::Main, 0, SlotSize::Fill(1)).await;
    let (_, aux) = add_pane(&ctx, &tui, "search", Slot::Aux, 0, SlotSize::Cells(6)).await;

    let size = Rect::new(0, 0, 80, 24);
    let before = layout(size, &tui.panes(), 1, 0);
    let main_before = before
        .iter()
        .find(|(p, _)| p.as_str() == "focus")
        .unwrap()
        .1;
    assert_eq!(main_before.height, 23 - 6);

    // The SWAP: the row that registered the Aux pane unloads.
    aux.dispose().await;

    assert_eq!(tui.panes().len(), 1, "the pane left with its row");
    let after = layout(size, &tui.panes(), 1, 0);
    let main_after = after.iter().find(|(p, _)| p.as_str() == "focus").unwrap().1;
    assert_eq!(
        main_after.height, 23,
        "the Aux slot took no space once it had no panes"
    );
    assert!(tui.hit_at(&PaneId::new("search"), 0, 0).is_none());
}

#[tokio::test]
async fn a_resize_relayouts_without_losing_pane_state() {
    let (ctx, tui) = shell();
    let (_rail, _) = add_pane(&ctx, &tui, "rail", Slot::Strip, 0, SlotSize::Cells(20)).await;
    let (focus, _) = add_pane(&ctx, &tui, "focus", Slot::Main, 0, SlotSize::Fill(1)).await;

    bough_plugin_tui_shell::run::draw(&tui);
    assert_eq!(tui.rect_of(&PaneId::new("focus")).unwrap().width, 60);
    // Give both panes some state, so "without losing" is a claim about something.
    bough_plugin_tui_shell::run::route(
        &tui,
        PaneId::new("focus"),
        bough_plugin_tui_shell::PaneEvent::Scroll { delta: 7 },
    )
    .await;
    assert_eq!(focus.scrolled(), 7);

    tui.resize(120, 40);
    bough_plugin_tui_shell::run::draw(&tui);

    assert_eq!(tui.size(), Rect::new(0, 0, 120, 40));
    assert_eq!(tui.rect_of(&PaneId::new("rail")).unwrap().width, 20);
    assert_eq!(tui.rect_of(&PaneId::new("focus")).unwrap().width, 100);
    assert_eq!(
        focus.scrolled(),
        7,
        "the pane kept its state across the resize"
    );
}

/// The one snapshot this package ships: the empty three-slot layout, so a change to the geometry
/// has to be looked at rather than merely compiled.
#[tokio::test]
async fn the_empty_three_slot_layout_is_stable() {
    let (ctx, tui) = shell();
    add_pane(&ctx, &tui, "rail", Slot::Strip, 0, SlotSize::Cells(16)).await;
    add_pane(&ctx, &tui, "focus", Slot::Main, 0, SlotSize::Fill(1)).await;
    add_pane(&ctx, &tui, "search", Slot::Aux, 0, SlotSize::Cells(4)).await;

    let rects: Vec<String> = layout(Rect::new(0, 0, 40, 12), &tui.panes(), 1, 0)
        .into_iter()
        .map(|(id, r)| format!("{id} {},{} {}x{}", r.x, r.y, r.width, r.height))
        .collect();
    insta::assert_debug_snapshot!(rects);
}

/// Keyboard focus at boot is the TRAJECTORY, not whichever row registered first.
///
/// `tui.strip` registers before `tui.focus` (bundle order), and a rule that read "the first
/// focusable pane" handed the rail every key for the whole session — which is why PageUp/PageDown
/// paged nothing at the screen (V3).
#[tokio::test]
async fn keyboard_focus_defaults_to_the_main_slot_however_the_rows_registered() {
    let (ctx, tui) = shell();
    add_pane(&ctx, &tui, "rail", Slot::Strip, 0, SlotSize::Cells(20)).await;
    assert_eq!(
        tui.focused_pane(),
        PaneId::new("rail"),
        "with only a rail, the rail is all there is"
    );

    add_pane(&ctx, &tui, "focus", Slot::Main, 0, SlotSize::Fill(1)).await;
    assert_eq!(
        tui.focused_pane(),
        PaneId::new("focus"),
        "the Main pane takes the default focus when it arrives"
    );

    add_pane(&ctx, &tui, "search", Slot::Aux, 0, SlotSize::Cells(4)).await;
    assert_eq!(
        tui.focused_pane(),
        PaneId::new("focus"),
        "a later Aux pane does not steal it"
    );
}

/// Once something CHOOSES a pane, a later registration never moves the focus again.
#[tokio::test]
async fn a_chosen_focus_survives_a_later_registration() {
    let (ctx, tui) = shell();
    add_pane(&ctx, &tui, "rail", Slot::Strip, 0, SlotSize::Cells(20)).await;
    tui.focus_pane(PaneId::new("rail")).await;

    add_pane(&ctx, &tui, "focus", Slot::Main, 0, SlotSize::Fill(1)).await;
    assert_eq!(tui.focused_pane(), PaneId::new("rail"));
}
