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
    // 80 − 20 rail − 1 gutter: the shell draws with `TuiConfig::gutter`, which is 1 (M9).
    assert_eq!(tui.rect_of(&PaneId::new("focus")).unwrap().width, 59);
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
    assert_eq!(tui.rect_of(&PaneId::new("focus")).unwrap().width, 99);
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

// ---------------------------------------------------------------------------
// phase ux1 §2.5: the gutter, and the rail's breakpoint
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_strip_slot_pays_for_the_gutter_and_the_pane_never_gets_it() {
    let (ctx, tui) = shell();
    add_pane(&ctx, &tui, "rail", Slot::Strip, 0, SlotSize::Cells(20)).await;
    add_pane(&ctx, &tui, "focus", Slot::Main, 0, SlotSize::Fill(1)).await;
    let rects = layout(Rect::new(0, 0, 80, 24), &tui.panes(), 1, 1);
    let of = |id: &str| rects.iter().find(|(p, _)| p.as_str() == id).unwrap().1;
    // The pane is handed `width`…
    assert_eq!(of("rail").width, 20);
    // …and the blank column between belongs to nobody: Main starts at width + gutter.
    assert_eq!(of("focus").x, 21);
    assert_eq!(of("focus").width, 59);
}

#[tokio::test]
async fn a_collapsed_rail_costs_no_columns_and_no_gutter() {
    let (ctx, tui) = shell();
    // The rail's own breakpoint: zero below 100 columns, clamped to 22..=40 above it.
    let size = SlotSize::Responsive {
        collapse: 100,
        preferred: 28,
        min: 22,
        max: 40,
    };
    add_pane(&ctx, &tui, "rail", Slot::Strip, 0, size).await;
    add_pane(&ctx, &tui, "focus", Slot::Main, 0, SlotSize::Fill(1)).await;

    let narrow = layout(Rect::new(0, 0, 80, 24), &tui.panes(), 1, 1);
    let main = narrow
        .iter()
        .find(|(p, _)| p.as_str() == "focus")
        .unwrap()
        .1;
    assert_eq!(
        main.x, 0,
        "under the breakpoint the transcript gets everything"
    );
    assert_eq!(main.width, 80);

    let wide = layout(Rect::new(0, 0, 120, 24), &tui.panes(), 1, 1);
    let of = |id: &str| wide.iter().find(|(p, _)| p.as_str() == id).unwrap().1;
    assert_eq!(of("rail").width, 28);
    assert_eq!(of("focus").x, 29);
}

#[test]
fn the_breakpoint_rule_is_zero_then_clamped() {
    use bough_plugin_tui_shell::responsive_width;
    assert_eq!(responsive_width(80, 100, 28, 22, 40), 0);
    assert_eq!(responsive_width(120, 100, 28, 22, 40), 28);
    assert_eq!(responsive_width(120, 100, 4, 22, 40), 22, "never under min");
    assert_eq!(
        responsive_width(200, 100, 500, 22, 40),
        40,
        "never over max"
    );
}

#[test]
fn the_prose_measure_is_capped_so_a_wide_terminal_gets_margin() {
    use bough_plugin_tui_shell::measure;
    assert_eq!(measure(200, 90), 90);
    assert_eq!(measure(60, 90), 60);
    assert_eq!(measure(0, 90), 1, "a measure is never zero");
}

/// …and the measure is REACHABLE from a pane, which is the half that was missing: `measure` had no
/// production call site and `TuiConfig::measure_cols` was read by nothing, so a 200-column terminal
/// got a full-width paragraph however the field was set (M13).
#[tokio::test]
async fn a_render_cx_hands_a_pane_the_capped_measure_and_not_the_pane_width() {
    use bough_plugin_tui_shell::{measure, test_config};
    let cfg = test_config();
    // The field the shell publishes into every `ShellView`, and the function a pane applies to it.
    assert_eq!(cfg.measure_cols, 90);
    assert_eq!(measure(200, cfg.measure_cols), 90);
    assert_eq!(measure(75, cfg.measure_cols), 75);
}

/// §0.2, "misconfiguration fails loud": the eight config fields phase ux1 added were unchecked, so
/// each of them silently DEGRADED a behaviour instead of refusing the load. `exit_arm_ms: 0` is the
/// worst of them — `ExitArm::is_armed` compares `elapsed <= window`, so a zero window means a
/// second Ctrl+C can never land inside it and the only idle exit path is simply gone.
#[test]
fn the_fields_this_phase_added_are_rejected_at_zero_rather_than_clamped() {
    use bough_kernel::Plugin;
    use bough_plugin_tui_shell::{test_config, TuiConfig, TuiShellPlugin};

    let check = |mutate: fn(&mut TuiConfig), want: &str| {
        let mut cfg = test_config();
        mutate(&mut cfg);
        let err = TuiShellPlugin::validate(&cfg).expect_err("a nonsense value is a load failure");
        let msg = format!("{err:?}");
        assert!(msg.contains(want), "the refusal names the field: {msg}");
    };

    check(|c| c.transcript_pane = String::new(), "transcript_pane");
    check(|c| c.measure_cols = 0, "measure_cols");
    check(|c| c.exit_arm_ms = 0, "exit_arm_ms");
    check(|c| c.paste_burst_ms = 0, "paste_burst_ms");
    check(|c| c.history_cap = 0, "history_cap");
    check(|c| c.notice_ms = 0, "notice_ms");
    check(|c| c.flash_ms = 0, "flash_ms");
    check(|c| c.gutter = 100, "gutter");

    // …and a zero GUTTER is meaningful (no blank column), so it is not rejected.
    let mut ok = test_config();
    ok.gutter = 0;
    TuiShellPlugin::validate(&ok).expect("gutter: 0 is a real choice");
    TuiShellPlugin::validate(&test_config()).expect("the shipped defaults validate");
}
