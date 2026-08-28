//! Invariant: ONE task owns the screen. Every draw, every hit map and every `last_frame` publish
//! happens in this loop, so no two writers can interleave escape sequences. A panic inside a
//! pane's render unwinds this task; the panic hook has already restored the terminal, and the
//! loop asks the kernel to exit with code 101 so the launcher tears the tree down (V8).

use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{Context, EffectCtx};
use bough_plugin_agents::{CancelCause, MailClass, Message, MessageId, Sender};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures::StreamExt;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Clear, Paragraph, Widget};

use crate::composer::ComposerAction;
use crate::events::{KeyDispatch, TuiKeyEvent};
use crate::pane::{self, HitMap, PaneEvent, PaneFrame, PaneId, PaneOutcome, RenderCx, Slot};
use crate::{no_pane, FocusRequest, TuiConfig, TuiHandle};

/// The event loop, spawned as the row's effect. Returns when the effect is halted.
pub async fn run(ctx: Context, tui: TuiHandle, cfg: Arc<TuiConfig>, e: EffectCtx) {
    let _ = ctx;
    // A headless backend has no stdin to read: the shell-use scripts drive a REAL terminal, and a
    // test drives the shell through `on_key` / `on_mouse` directly.
    let mut events = match tui.backend() {
        crate::Backend::Crossterm => Some(crossterm::event::EventStream::new()),
        _ => None,
    };
    let mut ticks = tokio::time::interval(Duration::from_millis(cfg.tick_ms));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    draw(&tui);
    loop {
        if e.is_halted() {
            return;
        }
        tokio::select! {
            biased;
            ev = async {
                match events.as_mut() {
                    Some(s) => s.next().await,
                    None => std::future::pending().await,
                }
            } => {
                match ev {
                    Some(Ok(ev)) => on_event(&tui, ev).await,
                    Some(Err(err)) => {
                        tracing::warn!(error = %err, "terminal event stream failed");
                        return;
                    }
                    None => return,
                }
                draw(&tui);
            }
            _ = tui.0.redraw.notified() => {
                // Coalesce: everything asked for in this budget costs one frame.
                tokio::time::sleep(Duration::from_millis(cfg.frame_ms)).await;
                draw(&tui);
            }
            _ = ticks.tick() => {
                // P3-D22: the roster is raised after the shell mounts, so the boot focus is
                // resolved here rather than at startup. A no-op once an agent is focused.
                tui.adopt_default_agent().await;
                // MERGE (note 16): and a submit that arrived before the roster goes NOW.
                flush_pending_send(&tui).await;
                for entry in tui.entries() {
                    let cx = tui.pane_cx();
                    let _ = entry.pane.handle(PaneEvent::Tick, cx).await;
                }
                draw(&tui);
            }
        }
    }
}

/// One terminal event, routed.
pub async fn on_event(tui: &TuiHandle, ev: Event) {
    match ev {
        Event::Key(key) => on_key(tui, key).await,
        Event::Mouse(me) => on_mouse(tui, me).await,
        Event::Paste(text) => on_paste(tui, &text),
        Event::Resize(..) => tui.redraw(),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Draw one frame: layout slots → each pane's `render` into a fresh `HitMap` → overlay the
/// selection highlight → publish `last_frame`.
pub fn draw(tui: &TuiHandle) {
    let now = chrono::Utc::now();
    let entries = tui.entries();
    let infos: Vec<_> = entries.iter().map(|e| e.info.clone()).collect();

    let mut terminal = tui.0.terminal.lock();
    let size = terminal.get_frame().area();
    let composer_h = tui.0.composer.lock().height(size.height / 2);
    // What the `Aux` panes reported LAST frame decides their rows THIS frame (visual audit F1):
    // the search pane collapses to nothing while it has no query and no hits, and comes back
    // when the keyboard is moved to it (`layout_with`'s focused rule).
    let previous = tui.0.reports.read().clone();
    let aux_rows: std::collections::HashMap<PaneId, u16> = previous
        .iter()
        .filter_map(|(id, r)| r.aux_rows.map(|n| (id.clone(), n)))
        .collect();
    // For the LAYOUT, "focused" means "has the keyboard": while the composer has it, no Aux
    // pane does, so a search pane that reported zero rows collapses instead of keeping the one
    // row the focused rule guarantees — the `search [▏]` ghost the power-user persona found
    // after Esc, Esc (the pane id stayed `focused_pane` while the keyboard had left it).
    let focused_pane = tui.focused_pane();
    let focused = layout_focus(&focused_pane, tui.composer_focused());
    let rects = pane::layout_with(
        size,
        &infos,
        composer_h,
        tui.0.cfg.gutter,
        focused,
        &aux_rows,
    );
    *tui.0.rects.write() = rects.clone();
    // The rows the notice band and the palette may borrow end where the Status band begins:
    // the status line is never painted over (visual audit F4).
    let status_top: u16 = rects
        .iter()
        .filter(|(id, _)| infos.iter().any(|p| p.id == *id && p.slot == Slot::Status))
        .map(|(_, r)| r.y)
        .min()
        .unwrap_or(u16::MAX);

    let selection = tui.selection().map(|s| s.rect());
    let sel_bg = tui.0.theme.sel_bg;
    let theme = tui.0.theme;
    tui.note_running(now);
    let notice = tui.notice_now(now);
    let palette_items = if tui.palette_open() {
        tui.commands().map(|c| {
            let query = tui.0.palette.lock().query.clone();
            (
                bough_plugin_commands::palette::filter(&c.list(None), &query),
                tui.0.palette.lock().selected,
            )
        })
    } else {
        None
    };
    let mut hits: std::collections::HashMap<PaneId, HitMap> = Default::default();
    let mut reports: std::collections::HashMap<PaneId, pane::RowReport> = Default::default();
    let mut published: Option<Buffer> = None;

    let _ = terminal.draw(|frame| {
        let buf = frame.buffer_mut();
        for (id, rect) in &rects {
            if rect.width == 0 || rect.height == 0 {
                hits.insert(id.clone(), HitMap::new());
                // A pane given no rows cannot report, so last frame's report stands — otherwise
                // a collapsed `Aux` pane would forget it asked for zero and spring back.
                if let Some(r) = previous.get(id) {
                    reports.insert(id.clone(), *r);
                }
                continue;
            }
            let Some(entry) = entries.iter().find(|e| e.info.id == *id) else {
                continue;
            };
            let view = tui.view(id, now, size);
            let mut map = HitMap::new();
            let mut report = pane::RowReport::default();
            {
                let mut cx = RenderCx {
                    frame: PaneFrame::new(buf),
                    area: *rect,
                    view: &view,
                    hits: &mut map,
                    report: &mut report,
                };
                entry.pane.render(&mut cx);
            }
            hits.insert(id.clone(), map);
            reports.insert(id.clone(), report);
        }

        // The composer is the shell's own, and it is drawn last so no pane can paint over it.
        let crect = pane::composer_rect(size, composer_h);
        if crect.height > 0 {
            let view = tui.view(&no_pane(), now, size);
            let mut map = HitMap::new();
            let mut report = pane::RowReport::default();
            let mut cx = RenderCx {
                frame: PaneFrame::new(buf),
                area: crect,
                view: &view,
                hits: &mut map,
                report: &mut report,
            };
            tui.0.composer.lock().render(&mut cx);
        }

        // The notice band. INTEGRATION SEAM (P3-D23): `notify` had a setter and a getter and no
        // reader in `draw`, so every notice — a `ctx.commands` result, an unknown-command error,
        // a copy confirmation, "no search pane" — was written to a field nobody painted, and V5's
        // "a slash command renders its output" could only ever pass by accident on a word that
        // was already in the rail. It is an OVERLAY, drawn after the panes and before the
        // selection: the shell owns no slot of its own, and a notice is ephemeral, so it borrows
        // the rows immediately above the composer rather than reflowing the layout under it.
        if let Some(notice) = notice {
            let text = notice.text.clone();
            // `pane::notice_band` decides WHAT is painted: the cap, the rows actually available
            // above the composer, and — when the two together drop lines — the marker that says
            // so. See its doc comment for why a silent truncation is not an option here.
            let floor = status_top.min(crect.y);
            let body_text = pane::notice_band_from(
                &text,
                tui.0.cfg.notice_max_lines,
                floor.saturating_sub(size.y),
                tui.0
                    .notice_scroll
                    .load(std::sync::atomic::Ordering::Relaxed),
                size.width,
            );
            let h = body_text.len() as u16;
            if h > 0 {
                let rect = Rect {
                    x: size.x,
                    y: floor - h,
                    width: size.width,
                    height: h,
                };
                // The ROLE decides the colour: an error reads like an error (M22).
                let fg = match notice.kind {
                    crate::NoticeKind::Error => theme.error,
                    crate::NoticeKind::Config => theme.evidence,
                    crate::NoticeKind::Copied => theme.accent,
                    crate::NoticeKind::Command => theme.fg,
                    crate::NoticeKind::Info => theme.fg,
                };
                // On its own ground (visual audit): a command's output used to sit on the
                // transcript's colour directly above the composer, so where the answer ended
                // and the output began was a guess. The band is `field_bg`, the composer's.
                let style = Style::default().fg(fg).bg(theme.field_bg);
                // A command's output is sections (`/help`: keys, each pane, commands): a line
                // that starts at the margin is a heading and reads bold, so the eye finds the
                // section it wants in one pass; the indented rows under it stay plain.
                let heading = style.add_modifier(ratatui::style::Modifier::BOLD);
                let is_command = matches!(notice.kind, crate::NoticeKind::Command);
                let body: Vec<Line> = body_text
                    .into_iter()
                    .map(|l| {
                        let lead = is_command
                            && !l.is_empty()
                            && !l.starts_with(' ')
                            && !l.starts_with('\u{2026}');
                        Line::styled(l, if lead { heading } else { style })
                    })
                    .collect();
                Clear.render(rect, buf);
                Paragraph::new(body).style(style).render(rect, buf);
            }
        }

        // The `/` palette. An OVERLAY for the same reason the notice is one: it is ephemeral,
        // and reflowing the layout under a filtering list would move the transcript on every
        // keystroke. It is sized to its content — it never reserves rows it has no content for.
        if let Some((items, selected)) = palette_items {
            let floor = status_top.min(crect.y);
            let max_rows = floor.saturating_sub(size.y).min(10);
            let lines = crate::palette::lines(&items, selected, size.width, max_rows, &theme);
            let h = lines.len() as u16;
            if h > 0 && floor >= h {
                let rect = Rect {
                    x: size.x,
                    y: floor - h,
                    width: size.width,
                    height: h,
                };
                Clear.render(rect, buf);
                Paragraph::new(lines)
                    .style(Style::default().bg(theme.bg))
                    .render(rect, buf);
            }
        }

        // The `to:` lane picker (round 5): a short list hanging off the chip's right edge, on the
        // rows above the band. Same geometry as the click test (`lane_picker_box`).
        if let Some((rect, rows, selected_row)) = lane_picker_box(tui, size, crect) {
            let body: Vec<Line> = rows
                .into_iter()
                .enumerate()
                .map(|(i, t)| {
                    let style = if Some(i) == selected_row {
                        Style::default().fg(theme.fg).bg(theme.sel_bg)
                    } else {
                        Style::default().fg(theme.fg).bg(theme.field_bg)
                    };
                    Line::styled(t, style)
                })
                .collect();
            Clear.render(rect, buf);
            Paragraph::new(body).render(rect, buf);
        }

        // The selection highlight sits ON TOP of everything: it is what the terminal would draw.
        if let Some(sel) = selection {
            let area = sel.intersection(buf.area);
            for y in area.y..area.y.saturating_add(area.height) {
                for x in area.x..area.x.saturating_add(area.width) {
                    buf[(x, y)].set_bg(sel_bg);
                }
            }
        }
        published = Some(buf.clone());
    });

    *tui.0.hits.write() = hits;
    *tui.0.reports.write() = reports;
    if let Some(buf) = published {
        *tui.0.last_frame.write() = Arc::new(buf);
    }
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

/// Route one already-typed pane event and act on its outcome (focus, command, compose).
pub async fn route(tui: &TuiHandle, target: PaneId, ev: PaneEvent) -> PaneOutcome {
    let Some(entry) = tui.entry(&target) else {
        return PaneOutcome::Ignored;
    };
    let outcome = entry.pane.handle(ev, tui.pane_cx()).await;
    match &outcome {
        PaneOutcome::Focus(req) => tui.focus(req.clone()).await,
        PaneOutcome::Command(line) => dispatch_line(tui, line).await,
        PaneOutcome::Compose(text) => {
            tui.set_composer_text(text);
            tui.focus_composer();
        }
        PaneOutcome::Handled => tui.redraw(),
        PaneOutcome::Ignored => {}
    }
    outcome
}

/// A bracketed paste always belongs to the composer: a pasted newline is text, never a send.
pub fn on_paste(tui: &TuiHandle, text: &str) {
    tui.0.composer.lock().on_paste(text);
    tui.focus_composer();
}

/// One key, through the whole chain: `tui/key` waterfall → [`crate::action_for`] → the palette,
/// the composer or the focused pane.
///
/// The keymap is consulted ONCE, on a snapshot of shell state read once. There is no second place
/// that reinterprets a key, which is what makes "who has focus" unable to change what PageUp does
/// (B1, B2).
pub async fn on_key(tui: &TuiHandle, key: KeyEvent) {
    if key.kind == KeyEventKind::Release {
        return;
    }
    let now = chrono::Utc::now();
    // BEFORE anything else (the sequencing rule of phase ux1 §2.3): the paste detector sees every
    // key, so an Enter arriving inside a burst is a newline in the draft and not a send (B4).
    // Only a REAL terminal can deliver a paste as keystrokes; a headless shell is driven by a
    // caller that means every key it sends, so the detector never fires there. Without this gate
    // a test (and a scripted PTY) would have every Enter swallowed as "part of a paste".
    // …and where the terminal DOES speak bracketed paste, `Event::Paste` already carries a paste
    // whole, so the timing heuristic is off: it could only ever mistake a fast typist (or a
    // scripted PTY) for one.
    let in_burst = tui.0.burst.lock().on_key(now)
        && tui.backend() == crate::Backend::Crossterm
        && !crate::term::bracketed_paste_active();

    // The extension point first (P3-D18): a listener that sets `handled` consumes the key.
    let dispatch = tui
        .0
        .ctx
        .waterfall::<TuiKeyEvent>(KeyDispatch {
            key,
            target: tui.focused_pane(),
            composer_focused: tui.composer_focused(),
            handled: false,
        })
        .await;
    if dispatch.handled {
        tui.redraw();
        return;
    }

    // PgUp/PgDn while a persistent notice is up SCROLL it (visual audit F4): `/help` is longer
    // than a screen, and a key that dismissed it was the only way past line forty.
    if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
        let page = i32::from(tui.0.cfg.page_lines);
        let delta = if key.code == KeyCode::PageUp {
            -page
        } else {
            page
        };
        let max = tui
            .notice_raw()
            .map(|n| {
                let size = tui.0.terminal.lock().get_frame().area();
                pane::notice_scroll_max(
                    &n.text,
                    tui.0.cfg.notice_max_lines,
                    size.height,
                    size.width,
                )
            })
            .unwrap_or(0);
        if tui.scroll_notice(delta, max) {
            return;
        }
    }
    // An ERROR notice has no TTL and waits for a key. This is that key.
    if matches!(tui.notice_raw().map(|n| n.ttl), Some(None)) {
        tui.clear_notice();
    }

    let cx = tui.key_context();
    let action = crate::action_for(key, cx, tui.0.cfg.page_lines);
    // Anything but a second Ctrl+C disarms: `press Ctrl+C again to exit` must not outlive a
    // change of mind (B7).
    if action != crate::Action::ExitStep {
        tui.disarm_exit();
    }

    match action {
        crate::Action::Scroll { delta } => {
            scroll_transcript(tui, delta).await;
            return;
        }
        crate::Action::JumpLatest => {
            scroll_transcript(tui, i16::MAX).await;
            return;
        }
        crate::Action::CycleFocus(step) => {
            cycle_focus(tui, step).await;
            return;
        }
        crate::Action::Interrupt => {
            interrupt(tui).await;
            return;
        }
        crate::Action::DismissOverlay => {
            dismiss_overlay(tui).await;
            return;
        }
        crate::Action::ExitStep => {
            exit_step(tui, now).await;
            return;
        }
        crate::Action::Help => {
            let prefix = tui.0.composer.lock().prefix();
            dispatch_line(tui, &format!("{prefix}help")).await;
            return;
        }
        crate::Action::FocusSearch => {
            focus_search(tui).await;
            return;
        }
        crate::Action::Redraw => {
            let _ = tui.0.terminal.lock().clear();
            tui.redraw();
            return;
        }
        crate::Action::Pass => {}
    }

    // The palette owns Up/Down/Tab/Enter while it is open, and nothing else.
    if lane_picker_key(tui, key).await {
        return;
    }
    if tui.palette_open() && palette_key(tui, key).await {
        return;
    }

    // B1: any printable key takes the keyboard back to the composer — UNLESS the pane holding
    // the keyboard is itself a text field (the search query), which is recognised by its taking
    // the key. So typing at a focused TRANSCRIPT lands in the draft (the audit's finding), while
    // `Ctrl+F` then typing still fills the search box.
    if !tui.composer_focused() && crate::snaps_to_composer(&key) {
        let target = tui.focused_pane();
        if target != no_pane()
            && route(tui, target, PaneEvent::Key(key)).await == PaneOutcome::Handled
        {
            tui.redraw();
            return;
        }
        tui.give_keyboard_to_composer().await;
    }

    if tui.composer_focused() {
        let action = tui.0.composer.lock().on_key(key, in_burst);
        match action {
            ComposerAction::Send(text) => {
                tui.0.composer.lock().clear();
                tui.0.burst.lock().reset();
                send(tui, &text).await
            }
            ComposerAction::Command(line) => dispatch_line(tui, &line).await,
            ComposerAction::Cleared | ComposerAction::Newline | ComposerAction::None => {
                tui.redraw()
            }
        }
        // M17: a `/` at line start opens the filtering palette, and every later keystroke narrows
        // it. It is driven from the DRAFT rather than from the key, so backspacing back to `/`
        // reopens it and backspacing past it closes it — the list can never disagree with the
        // line it is filtering.
        let draft = tui.0.composer.lock().text();
        let one_line = !draft.contains('\n');
        match draft.strip_prefix('/') {
            Some(rest) if one_line && !rest.starts_with('/') => tui.set_palette(true, rest),
            _ => {
                if tui.palette_open() {
                    tui.set_palette(false, "");
                }
            }
        }
        return;
    }

    let target = tui.focused_pane();
    if route(tui, target.clone(), PaneEvent::Key(key)).await == PaneOutcome::Ignored {
        if let Some(delta) = scroll_delta(key, tui.0.cfg.page_lines) {
            route(tui, target, PaneEvent::Scroll { delta }).await;
        }
    }
}

/// The paging keys drive the TRANSCRIPT, whatever has focus (B2). `transcript_pane` is matched
/// exactly; with no such pane the keys fall back to whatever does have the keyboard, so a tree
/// that renamed or disabled the row still scrolls something rather than nothing.
pub async fn scroll_transcript(tui: &TuiHandle, delta: i16) {
    let target = tui.transcript_pane().unwrap_or_else(|| tui.focused_pane());
    if target == no_pane() {
        return;
    }
    route(tui, target, PaneEvent::Scroll { delta }).await;
}

/// Esc / Ctrl+C while a turn is running: cancel it, and SAY SO where the user is looking (B7).
pub async fn interrupt(tui: &TuiHandle) {
    match tui.agent() {
        Some(a) => {
            a.cancel(CancelCause::User, true).await;
            tui.notify_kind("interrupted", crate::NoticeKind::Info);
        }
        None => tui.notify_kind("nothing is running", crate::NoticeKind::Info),
    }
}

/// PURE-ish: the open lane picker's box in screen coordinates, its row texts, and which row is
/// selected. `None` when the picker is closed or has nothing to list.
pub fn lane_picker_box(
    tui: &TuiHandle,
    size: Rect,
    crect: Rect,
) -> Option<(Rect, Vec<String>, Option<usize>)> {
    let selected = tui.lane_picker()?;
    let names: Vec<String> = tui.lanes().iter().map(|a| a.name().to_string()).collect();
    if names.is_empty() {
        return None;
    }
    let lane = tui.agent().map(|a| a.name().to_string());
    let chips = crate::composer::chips(crect.width, lane.as_deref(), tui.running());
    let chip_x1 = chips
        .iter()
        .find(|c| c.kind == crate::composer::ChipKind::Lane)
        .map(|c| c.x1)
        .unwrap_or(crect.width);
    let avail = crect.y.saturating_sub(size.y);
    let (x0, rows) = crate::composer::lane_picker(crect.width, chip_x1, avail, &names, selected);
    if rows.is_empty() {
        return None;
    }
    let h = rows.len() as u16;
    let w = rows[0].chars().count() as u16;
    let rect = Rect {
        x: crect.x + x0,
        y: crect.y - h,
        width: w,
        height: h,
    };
    // Which visible row is the selected lane (the list may be cut from the top).
    let first = selected
        .saturating_sub((h as usize).saturating_sub(1))
        .min(names.len() - h as usize);
    Some((rect, rows, Some(selected - first)))
}

/// The lane picker's keys while it is open (round 5): Up/Down move, Enter focuses, Esc closes;
/// any other key closes it and is then handled as usual. Returns true when the key was eaten.
pub async fn lane_picker_key(tui: &TuiHandle, key: KeyEvent) -> bool {
    let Some(selected) = tui.lane_picker() else {
        return false;
    };
    let lanes = tui.lanes();
    match key.code {
        KeyCode::Up | KeyCode::Down if !lanes.is_empty() => {
            let last = lanes.len() - 1;
            let next = if key.code == KeyCode::Up {
                selected.saturating_sub(1)
            } else {
                (selected + 1).min(last)
            };
            tui.set_lane_picker(Some(next));
            true
        }
        KeyCode::Enter => {
            tui.set_lane_picker(None);
            if let Some(lane) = lanes.get(selected) {
                tui.focus(to_agent(lane.id().clone())).await;
            }
            true
        }
        KeyCode::Esc => {
            tui.set_lane_picker(None);
            true
        }
        _ => {
            tui.set_lane_picker(None);
            false
        }
    }
}

/// A composer chip, pressed (the TUI brief, D7). Each one is the SAME path its key takes.
pub async fn on_chip(tui: &TuiHandle, kind: crate::composer::ChipKind) {
    match kind {
        crate::composer::ChipKind::Send => {
            let text = tui.0.composer.lock().text();
            if text.trim().is_empty() {
                tui.notify_kind("nothing to send yet", crate::NoticeKind::Info);
                return;
            }
            if crate::composer::is_command(&text, tui.0.composer.lock().prefix()) {
                dispatch_line(tui, &text).await;
                return;
            }
            tui.0.composer.lock().clear();
            tui.0.burst.lock().reset();
            send(tui, &text).await;
        }
        crate::composer::ChipKind::Stop => interrupt(tui).await,
        crate::composer::ChipKind::Lane => {
            // The picker (round 5): a list of lanes to pick from, opened on the current one.
            // Predictable with five lanes where cycling was five clicks.
            let lanes = tui.lanes();
            if lanes.is_empty() {
                tui.notify_kind("no agents to address", crate::NoticeKind::Info);
                return;
            }
            if tui.lane_picker().is_some() {
                tui.set_lane_picker(None);
                return;
            }
            let current = tui.focused_agent();
            let at = current
                .as_ref()
                .and_then(|id| lanes.iter().position(|a| a.id() == id))
                .unwrap_or(0);
            tui.set_lane_picker(Some(at));
        }
    }
}

/// Esc with something up: dismiss the topmost overlay. With nothing up this is a deliberate
/// NO-OP — the draft is never destroyed (B3, V3).
pub async fn dismiss_overlay(tui: &TuiHandle) {
    if tui.lane_picker().is_some() {
        tui.set_lane_picker(None);
        return;
    }
    if tui.palette_open() {
        tui.set_palette(false, "");
        return;
    }
    if tui.notice_raw().is_some() {
        tui.clear_notice();
        tui.redraw();
        return;
    }
    // A search pane with something on screen closes on Esc from ANYWHERE (round 10,
    // keyboard-only): after Enter on a hit the keyboard has left it, and an Esc that went to
    // the composer left the hits up with no way out but Ctrl+F.
    if let Some(search) = tui
        .panes()
        .into_iter()
        .find(|p| p.id.as_str() == tui.0.cfg.search_pane)
        .map(|p| p.id)
    {
        let visible = tui
            .0
            .reports
            .read()
            .get(&search)
            .and_then(|r| r.aux_rows)
            .is_some_and(|n| n > 0);
        if visible && (tui.composer_focused() || tui.focused_pane() != search) {
            let _ = route(
                tui,
                search.clone(),
                PaneEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            )
            .await;
            tui.give_keyboard_to_composer().await;
            return;
        }
    }
    // A focused pane may own an overlay of its own (the branch picker, the search query): give it
    // the key before concluding that there is nothing to dismiss.
    let target = tui.focused_pane();
    if target != no_pane() && !tui.composer_focused() {
        let _ = route(
            tui,
            target,
            PaneEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        )
        .await;
        // …and then the keyboard comes back to the composer, whatever the pane did with the key.
        // Esc means one thing everywhere — "I am done here" — and a pane that ate the key while
        // silently keeping the keyboard is how a typed sentence ends up in the search box
        // (phase ux1 (a): one always-live composer).
        tui.give_keyboard_to_composer().await;
    }
}

/// `Ctrl+C`: interrupt while running, else arm, then leave (B7).
pub async fn exit_step(tui: &TuiHandle, now: chrono::DateTime<chrono::Utc>) {
    if tui.running() {
        interrupt(tui).await;
        return;
    }
    match tui.exit_step(now) {
        crate::ExitStep::Arm => {
            tui.notify_kind("press Ctrl+C again to exit", crate::NoticeKind::Info)
        }
        crate::ExitStep::Exit => tui.quit_with(0, farewell()),
    }
}

/// The one line printed after the terminal is restored. Spelled once, so `/quit` and `Ctrl+C`
/// cannot say different things.
pub fn farewell() -> &'static str {
    "bough: bye."
}

/// `Ctrl+F`.
pub async fn focus_search(tui: &TuiHandle) {
    match tui
        .panes()
        .into_iter()
        .find(|p| p.focusable && p.id.as_str() == tui.0.cfg.search_pane)
    {
        Some(p) => tui.focus_pane(p.id).await,
        // The row can be disabled by patch; the binding is then a no-op, not an error.
        None => tui.notify_kind("no search pane", crate::NoticeKind::Error),
    }
}

/// One key into the open palette. `true` when the palette consumed it.
async fn palette_key(tui: &TuiHandle, key: KeyEvent) -> bool {
    use bough_plugin_commands::palette::{self, PaletteAction};
    let Some(commands) = tui.commands() else {
        return false;
    };
    let items = {
        let p = tui.0.palette.lock();
        palette::filter(&commands.list(None), &p.query)
    };
    let out = {
        let mut p = tui.0.palette.lock();
        palette::on_key(&mut p, key, &items)
    };
    match out {
        PaletteAction::None => false,
        PaletteAction::Moved | PaletteAction::Close => {
            tui.redraw();
            true
        }
        PaletteAction::Complete(name) => {
            let prefix = tui.0.composer.lock().prefix();
            tui.set_composer_text(&format!("{prefix}{name} "));
            true
        }
        PaletteAction::Accept(name) => {
            let prefix = tui.0.composer.lock().prefix();
            dispatch_line(tui, &format!("{prefix}{name}")).await;
            true
        }
    }
}

/// Move keyboard focus one step over the focusable panes and the composer.
async fn cycle_focus(tui: &TuiHandle, step: i32) {
    // The conversation first, the rail last (round 6): Tab's first stop used to be the rail,
    // where the arrows did nothing visible — a dead stop before the pane a person wants.
    let mut panes = tui.panes();
    panes.sort_by_key(|p| p.slot == crate::pane::Slot::Strip);
    // …and a pane with no rows on screen (a collapsed search pane) is not a stop: Tab landing
    // on nothing visible was the "second Tab does nothing" the power-user persona hit. Ctrl+F
    // is the way into search.
    let rects = tui.0.rects.read().clone();
    let visible = |id: &PaneId| {
        rects
            .iter()
            .any(|(r, rect)| r == id && rect.height > 0 && rect.width > 0)
    };
    let mut stops: Vec<Option<PaneId>> = panes
        .into_iter()
        .filter(|p| p.focusable && visible(&p.id))
        .map(|p| Some(p.id))
        .collect();
    stops.push(None); // the composer
    let here = if tui.composer_focused() {
        stops.len() - 1
    } else {
        let focused = tui.focused_pane();
        stops
            .iter()
            .position(|s| s.as_ref() == Some(&focused))
            .unwrap_or(stops.len() - 1)
    };
    let len = stops.len() as i32;
    let next = (((here as i32 + step) % len) + len) % len;
    match &stops[next as usize] {
        Some(id) => tui.focus_pane(id.clone()).await,
        None => tui.focus_composer(),
    }
}

/// The scroll a navigation key means, in lines, at the shell's configured page size. `None` for a
/// key that is not one.
pub fn scroll_delta(key: KeyEvent, page: u16) -> Option<i16> {
    let page = page.clamp(1, i16::MAX as u16) as i16;
    match key.code {
        KeyCode::Up => Some(-1),
        KeyCode::Down => Some(1),
        KeyCode::PageUp => Some(-page),
        KeyCode::PageDown => Some(page),
        KeyCode::Home => Some(i16::MIN),
        KeyCode::End => Some(i16::MAX),
        _ => None,
    }
}

/// Mouse: click focuses and forwards its hit, wheel scrolls WITHOUT moving focus, drag selects.
pub async fn on_mouse(tui: &TuiHandle, me: MouseEvent) {
    let (col, row) = (me.column, me.row);
    match me.kind {
        MouseEventKind::Down(button) => {
            // The lane picker, when open, takes the click: a row picks that lane; anywhere
            // else closes it and the click goes on to whatever it landed on.
            if tui.lane_picker().is_some() {
                let size = tui.size();
                let crect = composer_rect(tui);
                if let Some((rect, _, _)) = lane_picker_box(tui, size, crect) {
                    if col >= rect.x
                        && col < rect.x + rect.width
                        && row >= rect.y
                        && row < rect.y + rect.height
                    {
                        let names = tui.lanes();
                        let h = rect.height as usize;
                        let selected = tui.lane_picker().unwrap_or(0);
                        let first = selected
                            .saturating_sub(h.saturating_sub(1))
                            .min(names.len().saturating_sub(h));
                        let picked = first + (row - rect.y) as usize;
                        tui.set_lane_picker(None);
                        if let Some(lane) = names.get(picked) {
                            tui.focus(to_agent(lane.id().clone())).await;
                        }
                        return;
                    }
                }
                tui.set_lane_picker(None);
                // A click on the `to:` chip itself while the list was open CLOSES it and does
                // nothing else — the chip is a toggle, not a reopen.
                let area = composer_rect(tui);
                let lane = tui.agent().map(|a| a.name().to_string());
                let chips = crate::composer::chips(area.width, lane.as_deref(), tui.running());
                let on_last_row = row == area.y + area.height.saturating_sub(1);
                if on_last_row
                    && col
                        .checked_sub(area.x)
                        .and_then(|c| crate::composer::chip_at(&chips, c))
                        .is_some_and(|c| c.kind == crate::composer::ChipKind::Lane)
                {
                    return;
                }
            }
            let Some(pane) = tui.pane_at(col, row) else {
                // A click on the composer's band: a CHIP if one is under the pointer (D7) —
                // the same geometry `render_at` drew — else focus, with the caret where the
                // pointer landed (minor 33).
                let area = composer_rect(tui);
                let lane = tui.agent().map(|a| a.name().to_string());
                let chips = crate::composer::chips(area.width, lane.as_deref(), tui.running());
                let on_last_row = row == area.y + area.height.saturating_sub(1);
                let hit = col
                    .checked_sub(area.x)
                    .filter(|_| on_last_row)
                    .and_then(|c| crate::composer::chip_at(&chips, c).cloned());
                if let Some(chip) = hit {
                    on_chip(tui, chip.kind).await;
                    return;
                }
                tui.0.composer.lock().caret_at(col, row, area);
                tui.focus_composer();
                return;
            };
            // B1: a click on a pane ACTS on the row it landed on and does NOT move the keyboard.
            // Focus is moved by Tab, by Ctrl+F and by a pane that asks for it — never by a click,
            // because a click is how a user reads, and reading must not silently redirect typing.
            *tui.0.selection.lock() = Some(crate::Selection {
                anchor: (col, row),
                head: (col, row),
            });
            let hit = tui.hit_at(&pane, col, row);
            route(
                tui,
                pane,
                PaneEvent::Click {
                    at: (col, row),
                    hit,
                    button,
                    clicks: 1,
                },
            )
            .await;
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let mut sel = tui.0.selection.lock();
            if let Some(s) = sel.as_mut() {
                s.head = (col, row);
            }
            drop(sel);
            tui.redraw();
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let selection = tui.selection();
            if let Some(s) = selection {
                if !s.is_empty() {
                    let text = crate::text_from_buffer(&tui.last_frame(), s.rect());
                    tui.copy(&text).await;
                } else {
                    // A click that never dragged is not a selection (visual audit): the one
                    // highlighted cell it left behind was chrome for nothing, sitting wherever
                    // the pointer last landed.
                    *tui.0.selection.lock() = None;
                    tui.redraw();
                }
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let notch = tui.0.cfg.wheel_lines.clamp(1, i16::MAX as u16) as i16;
            let delta = if me.kind == MouseEventKind::ScrollUp {
                -notch
            } else {
                notch
            };
            // Focus is deliberately untouched: a wheel over another pane reads it, it does not
            // select it. Over the composer band or a zero-size slot the wheel still means "scroll
            // the conversation" (M23), which is the only thing a wheel can sensibly mean there.
            // A wheel over the RAIL scrolls the conversation too (round 8): the rail has nothing
            // to scroll, and a wheel that does nothing over a third of the screen reads as broken.
            let is_strip = |id: &PaneId| {
                tui.panes()
                    .iter()
                    .any(|p| &p.id == id && p.slot == crate::pane::Slot::Strip)
            };
            match tui.pane_at(col, row) {
                Some(pane) if !is_strip(&pane) => {
                    route(tui, pane, PaneEvent::Scroll { delta }).await;
                }
                _ => scroll_transcript(tui, delta).await,
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// The two things Enter can mean
// ---------------------------------------------------------------------------

/// Plain text: a followup to the focused agent, as an ANDREY message (§5's answer-wake rule).
///
/// MERGE (note 16): the pre-ready case QUEUES rather than bouncing. `residents` raises the roster
/// asynchronously after the shell mounts, so on a cold boot there is a real window — about one
/// submit in three, measured against the release binary — in which Enter has nobody to send to.
/// Handing the text back with "no focused agent" is honest but useless: the person did nothing
/// wrong and has to type Enter again. The message now waits in `pending_send` and the tick sends
/// it the moment an agent exists ([`flush_pending_send`]).
pub async fn send(tui: &TuiHandle, text: &str) {
    // P3-D22: the tick adopts the boot focus, but a message typed in the first tick window would
    // otherwise wait a whole tick for the timer. Ask for the adoption here rather than racing it.
    if tui.agent().is_none() {
        tui.adopt_default_agent().await;
    }
    let Some(agent) = tui.agent() else {
        if tui.queue_send(text) {
            tui.notify_kind(
                "waiting for an agent — your message is queued",
                crate::NoticeKind::Info,
            );
        } else {
            // Something is already queued. The second message is given straight back rather than
            // replacing the first: nothing the user typed is destroyed (B3).
            tui.notify_kind("still waiting for an agent", crate::NoticeKind::Error);
            tui.set_composer_text(text);
        }
        return;
    };
    let now = chrono::Utc::now();
    let msg = Message {
        id: MessageId::new(uuid::Uuid::now_v7().to_string()),
        from: Sender::Andrey,
        class: MailClass::Wake,
        text: text.to_string(),
        subject: subject_of(text),
        cites: Vec::new(),
        refs: Default::default(),
        mail_seq: None,
        at: now,
    };
    match agent.followup(msg).await {
        Ok(_) => tui.redraw(),
        Err(e) => tui.notify(format!("send failed: {e}")),
    }
}

/// Send whatever was queued by a pre-ready submit, once there is somebody to send it to
/// (merge note 16).
///
/// Called from the tick, which is also where the boot focus is adopted — so the queued message
/// goes in the same pass that first finds an agent. Past [`crate::PENDING_SEND_TICKS`] the text is
/// handed back to the composer with an error: a tree with no agents at all must SAY so rather than
/// hold a message forever.
pub async fn flush_pending_send(tui: &TuiHandle) {
    if tui.pending_send().is_none() {
        return;
    }
    if tui.agent().is_some() {
        if let Some(p) = tui.take_pending_send() {
            tui.clear_notice();
            send(tui, &p.text).await;
        }
        return;
    }
    if tui.bump_pending_send() >= crate::PENDING_SEND_TICKS {
        if let Some(p) = tui.take_pending_send() {
            tui.notify_kind(
                "no agent came up; your message is back in the composer",
                crate::NoticeKind::Error,
            );
            tui.set_composer_text(&p.text);
        }
    }
}

/// PURE: the pane the layout treats as focused — the focused pane only while the keyboard is
/// there, never while the composer has it.
pub fn layout_focus(focused_pane: &PaneId, composer_focused: bool) -> Option<&PaneId> {
    if composer_focused {
        None
    } else {
        Some(focused_pane)
    }
}

/// A slash line: through `ctx.commands`, and NEVER to an agent (V5). Appends no step (P3-D8).
///
/// THE TEXT IS NOT DESTROYED (B3). The composer is cleared only where the name RESOLVED; on a
/// miss the draft stays, `arm_send_as_message` is set, and the notice says both what was near and
/// that a second Enter sends the line as a message.
pub async fn dispatch_line(tui: &TuiHandle, line: &str) {
    *tui.0.last_command.lock() = Some(line.to_string());
    let Some(commands) = tui.0.commands.clone() else {
        miss(tui, format!("no commands registry for `{line}`"));
        return;
    };
    let prefix = tui.0.composer.lock().prefix();
    let Some(inv) = bough_plugin_commands::parse(line, prefix) else {
        miss(tui, bough_plugin_commands::palette::miss_notice(line, None));
        return;
    };
    let cx = bough_plugin_commands::CommandCx {
        ctx: tui.0.ctx.clone(),
        agent: tui.agent(),
        at: chrono::Utc::now(),
    };
    let raw = line.to_string();
    // The event loop awaits this call, so a command that takes seconds (`/reconsolidate`,
    // `/seal`: model calls) used to FREEZE the frame — the composer looked unsent, the palette
    // looked open, and every key typed meanwhile landed on a screen that had not moved (visual
    // audit, 23-commands). A quick command still settles inline, so the frame after Enter shows
    // its answer; a slow one hands the loop back, says it is running, and settles from a task.
    let mut run = Box::pin(async move { commands.dispatch(inv, cx).await });
    match tokio::time::timeout(Duration::from_millis(INLINE_COMMAND_MS), &mut run).await {
        Ok(outcome) => settle(tui, &raw, outcome),
        Err(_) => {
            tui.notify_kind(
                format!("{raw}\nrunning\u{2026}"),
                crate::NoticeKind::Command,
            );
            let tui = tui.clone();
            tokio::spawn(async move {
                let outcome = run.await;
                settle(&tui, &raw, outcome);
            });
        }
    }
}

/// How long a slash command may hold the event loop before it becomes a background task.
pub const INLINE_COMMAND_MS: u64 = 120;

fn miss(tui: &TuiHandle, text: String) {
    tui.0.composer.lock().arm_send_as_message();
    tui.notify_kind(text, crate::NoticeKind::Error);
}

/// What a command's answer does to the shell, whichever side of the budget it arrived on.
fn settle(
    tui: &TuiHandle,
    raw: &str,
    outcome: Result<bough_plugin_commands::CommandOutput, bough_plugin_commands::CommandError>,
) {
    match outcome {
        Ok(out) => {
            // Resolved: NOW the line may go.
            tui.0.composer.lock().clear();
            tui.set_palette(false, "");
            tui.notify_kind(
                bough_plugin_commands::palette::echoed(raw, &out.text),
                crate::NoticeKind::Command,
            );
        }
        Err(bough_plugin_commands::CommandError::Unknown { name, did_you_mean }) => miss(
            tui,
            bough_plugin_commands::palette::miss_notice(&name, did_you_mean.as_deref()),
        ),
        Err(e) => {
            tui.0.composer.lock().clear();
            tui.notify_kind(e.to_string(), crate::NoticeKind::Error);
        }
    }
}

/// A one-line subject: the first line of the message, clipped.
pub fn subject_of(text: &str) -> String {
    const MAX: usize = 80;
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= MAX {
        line.to_string()
    } else {
        line.chars().take(MAX - 1).collect::<String>() + "\u{2026}"
    }
}

/// The rectangle the composer occupies. Re-exported here because the loop is what decides it.
pub fn composer_rect(tui: &TuiHandle) -> Rect {
    let size = tui.size();
    let h = tui.0.composer.lock().height(size.height / 2);
    pane::composer_rect(size, h)
}

/// A focus request that names only an agent.
pub fn to_agent(agent: bough_plugin_agents::AgentId) -> FocusRequest {
    FocusRequest {
        agent: Some(agent),
        ..Default::default()
    }
}

#[cfg(test)]
mod layout_focus_tests {
    use super::layout_focus;
    use crate::pane::PaneId;

    #[test]
    fn the_layout_sees_no_focused_pane_while_the_composer_has_the_keyboard() {
        let id = PaneId::new("tui.search");
        assert_eq!(layout_focus(&id, false), Some(&id));
        assert_eq!(layout_focus(&id, true), None);
    }
}
