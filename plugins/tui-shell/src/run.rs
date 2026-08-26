//! Invariant: ONE task owns the screen. Every draw, every hit map and every `last_frame` publish
//! happens in this loop, so no two writers can interleave escape sequences. A panic inside a
//! pane's render unwinds this task; the panic hook has already restored the terminal, and the
//! loop asks the kernel to exit with code 101 so the launcher tears the tree down (V8).

use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{Context, EffectCtx};
use bough_plugin_agents::{CancelCause, MailClass, Message, MessageId, Sender, Status};
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
use crate::pane::{self, HitMap, PaneEvent, PaneFrame, PaneId, PaneOutcome, RenderCx};
use crate::{no_pane, FocusRequest, TuiConfig, TuiHandle};

/// How many rows a notice may borrow above the composer before it is truncated.
const NOTICE_MAX_LINES: usize = 8;

/// The event loop, spawned as the row's effect. Returns when the effect is halted.
pub async fn run(ctx: Context, tui: TuiHandle, cfg: Arc<TuiConfig>, e: EffectCtx) {
    let _ = ctx;
    // A headless backend has no stdin to read: the shell-use scripts drive a REAL terminal, and a
    // test drives the shell through `on_key` / `on_mouse` directly.
    let mut events = match tui.backend() {
        crate::Backend::Crossterm => Some(crossterm::event::EventStream::new()),
        _ => None,
    };
    let mut ticks = tokio::time::interval(Duration::from_millis(cfg.tick_ms.max(1)));
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
                tokio::time::sleep(Duration::from_millis(cfg.frame_ms.max(1))).await;
                draw(&tui);
            }
            _ = ticks.tick() => {
                // P3-D22: the roster is raised after the shell mounts, so the boot focus is
                // resolved here rather than at startup. A no-op once an agent is focused.
                tui.adopt_default_agent().await;
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
    let rects = pane::layout(size, &infos, composer_h);
    *tui.0.rects.write() = rects.clone();

    let selection = tui.selection().map(|s| s.rect());
    let sel_bg = tui.0.theme.sel_bg;
    let theme = tui.0.theme;
    let notice = tui.notice();
    let mut hits: std::collections::HashMap<PaneId, HitMap> = Default::default();
    let mut published: Option<Buffer> = None;

    let _ = terminal.draw(|frame| {
        let buf = frame.buffer_mut();
        for (id, rect) in &rects {
            if rect.width == 0 || rect.height == 0 {
                hits.insert(id.clone(), HitMap::new());
                continue;
            }
            let Some(entry) = entries.iter().find(|e| e.info.id == *id) else {
                continue;
            };
            let view = tui.view(id, now, size);
            let mut map = HitMap::new();
            {
                let mut cx = RenderCx {
                    frame: PaneFrame::new(buf),
                    area: *rect,
                    view: &view,
                    hits: &mut map,
                };
                entry.pane.render(&mut cx);
            }
            hits.insert(id.clone(), map);
        }

        // The composer is the shell's own, and it is drawn last so no pane can paint over it.
        let crect = pane::composer_rect(size, composer_h);
        if crect.height > 0 {
            let view = tui.view(&no_pane(), now, size);
            let mut map = HitMap::new();
            let mut cx = RenderCx {
                frame: PaneFrame::new(buf),
                area: crect,
                view: &view,
                hits: &mut map,
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
        if let Some(text) = notice {
            let wrapped: Vec<&str> = text.lines().collect();
            let want = wrapped.len().clamp(1, NOTICE_MAX_LINES) as u16;
            let top = crect.y.saturating_sub(want);
            let h = crect.y.saturating_sub(top);
            if h > 0 {
                let rect = Rect {
                    x: size.x,
                    y: top,
                    width: size.width,
                    height: h,
                };
                let style = Style::default().fg(theme.hint).bg(theme.bg);
                let body: Vec<Line> = wrapped
                    .iter()
                    .take(h as usize)
                    .map(|l| Line::styled((*l).to_string(), style))
                    .collect();
                Clear.render(rect, buf);
                Paragraph::new(body).style(style).render(rect, buf);
            }
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

/// One key, through the whole chain: `tui/key` waterfall → the keymap → the composer or the
/// focused pane.
pub async fn on_key(tui: &TuiHandle, key: KeyEvent) {
    if key.kind == KeyEventKind::Release {
        return;
    }
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

    if keymap(tui, key).await {
        return;
    }

    // PageUp/PageDown belong to the TRAJECTORY, not to the composer. The composer is a few lines
    // tall, has no page to turn, and holds keyboard focus for the whole session — so without this
    // the focused pane could never be paged from the keyboard at all (V3,
    // `page_up_and_arrow_keys_scroll_the_trajectory`). Up/Down/Home/End are deliberately NOT in
    // this set: those move the composer's own cursor, and the wheel and PageUp/PageDown are the
    // trajectory's.
    if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
        let target = tui.focused_pane();
        if route(tui, target.clone(), PaneEvent::Key(key)).await == PaneOutcome::Ignored {
            if let Some(delta) = scroll_delta(key) {
                route(tui, target, PaneEvent::Scroll { delta }).await;
            }
        }
        return;
    }

    if tui.composer_focused() {
        let action = tui.0.composer.lock().on_key(key);
        match action {
            ComposerAction::Send(text) => send(tui, &text).await,
            ComposerAction::Command(line) => dispatch_line(tui, &line).await,
            ComposerAction::Cleared | ComposerAction::None => tui.redraw(),
        }
        return;
    }

    let target = tui.focused_pane();
    if route(tui, target.clone(), PaneEvent::Key(key)).await == PaneOutcome::Ignored {
        if let Some(delta) = scroll_delta(key) {
            route(tui, target, PaneEvent::Scroll { delta }).await;
        }
    }
}

/// The fixed keymap (P3-D18). `true` means the key was consumed here.
async fn keymap(tui: &TuiHandle, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if ctrl => {
            // Cancel a running wake; with nothing running, quit.
            match tui.agent() {
                Some(a) if a.status() == Status::Running => {
                    a.cancel(CancelCause::User, true).await;
                    tui.notify("cancelled");
                }
                _ => tui.quit(0),
            }
            true
        }
        KeyCode::Char('l') if ctrl => {
            let _ = tui.0.terminal.lock().clear();
            tui.redraw();
            true
        }
        KeyCode::Char('f') if ctrl => {
            match tui
                .panes()
                .into_iter()
                .find(|p| p.focusable && p.id.as_str().contains("search"))
            {
                Some(p) => tui.focus_pane(p.id).await,
                // The row can be disabled by patch; the binding is then a no-op, not an error.
                None => tui.notify("no search pane"),
            }
            true
        }
        KeyCode::Tab => {
            cycle_focus(tui, 1).await;
            true
        }
        KeyCode::BackTab => {
            cycle_focus(tui, -1).await;
            true
        }
        KeyCode::Esc if !tui.composer_focused() => {
            tui.focus_composer();
            true
        }
        _ => false,
    }
}

/// Move keyboard focus one step over the focusable panes and the composer.
async fn cycle_focus(tui: &TuiHandle, step: i32) {
    let mut stops: Vec<Option<PaneId>> = tui
        .panes()
        .into_iter()
        .filter(|p| p.focusable)
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

/// The scroll a navigation key means, in lines. `None` for a key that is not one.
pub fn scroll_delta(key: KeyEvent) -> Option<i16> {
    match key.code {
        KeyCode::Up => Some(-1),
        KeyCode::Down => Some(1),
        KeyCode::PageUp => Some(-10),
        KeyCode::PageDown => Some(10),
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
            let Some(pane) = tui.pane_at(col, row) else {
                // A click on the composer's band gives it focus.
                tui.focus_composer();
                return;
            };
            tui.focus_pane(pane.clone()).await;
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
                }
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let delta = if me.kind == MouseEventKind::ScrollUp {
                -3
            } else {
                3
            };
            if let Some(pane) = tui.pane_at(col, row) {
                // Focus is deliberately untouched: a wheel over another pane reads it, it does
                // not select it.
                route(tui, pane, PaneEvent::Scroll { delta }).await;
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// The two things Enter can mean
// ---------------------------------------------------------------------------

/// Plain text: a followup to the focused agent, as an ANDREY message (§5's answer-wake rule).
pub async fn send(tui: &TuiHandle, text: &str) {
    // P3-D22: the tick adopts the boot focus, but a message typed in the first tick window would
    // otherwise be bounced back to the composer purely because the timer had not fired yet. Ask
    // for the adoption here rather than racing it; with a roster that is genuinely empty this
    // still falls through to the notice.
    if tui.agent().is_none() {
        tui.adopt_default_agent().await;
    }
    let Some(agent) = tui.agent() else {
        tui.notify("no focused agent");
        tui.set_composer_text(text);
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

/// A slash line: through `ctx.commands`, and NEVER to an agent (V5). Appends no step (P3-D8).
pub async fn dispatch_line(tui: &TuiHandle, line: &str) {
    *tui.0.last_command.lock() = Some(line.to_string());
    let Some(commands) = tui.0.commands.clone() else {
        tui.notify(format!("no commands registry for `{line}`"));
        return;
    };
    let prefix = tui.0.composer.lock().prefix();
    let Some(inv) = bough_plugin_commands::parse(line, prefix) else {
        tui.notify(format!("not a command: `{line}`"));
        return;
    };
    let cx = bough_plugin_commands::CommandCx {
        ctx: tui.0.ctx.clone(),
        agent: tui.agent(),
        at: chrono::Utc::now(),
    };
    match commands.dispatch(inv, cx).await {
        Ok(out) => tui.notify(out.text),
        Err(e) => tui.notify(e.to_string()),
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
