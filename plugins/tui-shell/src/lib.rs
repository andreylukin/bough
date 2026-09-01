//! Invariant: this crate OWNS THE TERMINAL, and nothing else in the tree touches it. It is the
//! only place that enters raw mode and the alt screen, the only place that draws, and the only
//! place that restores — on a clean quit, on a boot failure, on a panic and on SIGINT (§11, V8).
//! Panes are registered as EFFECTS: a pane row unloading reflows the layout with no restart, and
//! that is the phase's SWAP gate.
//!
//! It drives `ctx.agents` and reads `ctx.ledger`; it never imports `bough-plugin-agent-loop`.

pub mod backend;
pub mod builtins;
pub mod clip;
pub mod composer;
pub mod contrast;
pub mod draft;
pub mod events;
pub mod invariant;
pub mod keymap;
pub mod notice;
pub mod palette;
pub mod pane;
pub mod run;
pub mod select;
pub mod term;
pub mod theme;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use bough_kernel::{
    ConfigError, Context, EffectHandle, Inject, InvariantSpec, Plugin, PluginError, ServiceKey,
};
use bough_plugin_agents::{Agent, AgentId, Agents, AgentsHandle};
use bough_plugin_commands::{Commands, CommandsHandle};
use bough_plugin_ledger::StepId;
use parking_lot::{Mutex, RwLock};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::Terminal;
use tokio::sync::Notify;

pub use clip::{copy, CopyOutcome};
pub use composer::{Composer, ComposerAction};
pub use events::{FocusRequest, KeyDispatch, TuiFocusEvent, TuiKeyEvent};
pub use keymap::{
    action_for, hints, snaps_to_composer, Action, ExitArm, ExitStep, Focus, KeyContext,
};
pub use notice::{Notice, NoticeKind};
pub use pane::{measure, responsive_width, RowReport};
pub use pane::{
    HitId, HitMap, Pane, PaneCx, PaneEvent, PaneFrame, PaneId, PaneInfo, PaneOutcome, PaneSpec,
    RenderCx, ShellView, Slot, SlotSize,
};
pub use select::{text_from_buffer, Selection};
pub use term::{install_panic_hook, restore_now, TerminalGuard};
pub use theme::{Backend, Theme, ThemeName};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-shell";

/// The `tui` service key.
pub struct Tui;

impl ServiceKey for Tui {
    type Value = TuiHandle;
    const NAME: &'static str = "tui";
}

/// The id `focused_pane()` reports when no pane holds keyboard focus. A sentinel and not an
/// `Option` because every render wants to compare against it without unwrapping.
pub fn no_pane() -> PaneId {
    PaneId::new("-")
}

/// PURE: which pane keyboard focus falls to when nothing holds it — at boot, and when the pane
/// that held it is disposed.
///
/// [`Slot::Main`] wins. Registration order does NOT decide this: rows load in bundle order, so
/// `tui.strip` registers before `tui.focus` and the rail was silently taking every key — which is
/// why PageUp/PageDown paged nothing (V3, `page_up_and_arrow_keys_scroll_the_trajectory`). The
/// trajectory is what a key means when the user has not said otherwise; the rail is reached by
/// clicking it or by Tab.
pub fn default_focus(panes: &[PaneInfo]) -> Option<PaneId> {
    let pick = |slot: Slot| {
        crate::pane::sorted(panes)
            .into_iter()
            .find(|p| p.focusable && p.slot == slot)
            .map(|p| p.id.clone())
    };
    pick(Slot::Main).or_else(|| {
        crate::pane::sorted(panes)
            .into_iter()
            .find(|p| p.focusable)
            .map(|p| p.id.clone())
    })
}

/// A registered pane and the object behind it.
#[derive(Clone)]
pub(crate) struct PaneEntry {
    pub info: PaneInfo,
    pub pane: Arc<dyn Pane>,
}

/// The concrete handle the key's value is.
#[derive(Clone)]
pub struct TuiHandle(pub Arc<TuiInner>);

/// The shell's live state: the pane registry, the focus, the composer, the last frame.
pub struct TuiInner {
    pub(crate) ctx: Context,
    pub(crate) cfg: Arc<TuiConfig>,
    /// `Backend::Auto` already resolved (P3-D2).
    pub(crate) backend: Backend,
    pub(crate) theme: Theme,
    pub(crate) agents: Option<Arc<AgentsHandle>>,
    /// The ledger, for the built-ins that must read a durable fact the live registry does not
    /// carry — `/agents` reads dormancy by step-type NAME (visual audit F13; P3-D11's rule).
    pub(crate) ledger: Mutex<Option<Arc<bough_plugin_ledger::LedgerHandle>>>,
    pub(crate) commands: Option<Arc<CommandsHandle>>,
    pub(crate) terminal: Mutex<Terminal<backend::TermBackend>>,
    pub(crate) panes: RwLock<Vec<PaneEntry>>,
    pub(crate) rects: RwLock<Vec<(PaneId, Rect)>>,
    pub(crate) hits: RwLock<HashMap<PaneId, HitMap>>,
    /// What each pane REPORTED about its own roving state on the last frame (§2.12). The shell
    /// cannot read inside a pane, so `ShellView::row_focus` / `following` are fed from here.
    pub(crate) reports: RwLock<HashMap<PaneId, pane::RowReport>>,
    pub(crate) focused_agent: RwLock<Option<AgentId>>,
    pub(crate) focused_pane: RwLock<PaneId>,
    pub(crate) composer_focused: AtomicBool,
    /// Set the moment `quit` is asked for: panes stop touching the ledger from then on, so no
    /// read transaction can straddle the shutdown checkpoint (24-honesty's WAL check).
    pub(crate) quitting: AtomicBool,
    /// The `to:` lane picker (round 5): `Some(selected)` while open.
    pub(crate) lane_picker: Mutex<Option<usize>>,
    /// Set once anything CHOSE a pane (a click, Tab, a `FocusRequest`). Until then keyboard focus
    /// is only a default, and every new registration re-derives it — see `default_focus`.
    pub(crate) focus_chosen: AtomicBool,
    pub(crate) composer: Mutex<Composer>,
    pub(crate) selection: Mutex<Option<Selection>>,
    /// The rail width the user dragged the divider to, in columns; `None` until they do.
    /// Session-local: a preference held by the hand, not by config.
    pub(crate) rail_cols: RwLock<Option<u16>>,
    /// Whether a divider drag is in flight (mouse down on the gutter, not yet released).
    pub(crate) rail_drag: AtomicBool,
    pub(crate) last_frame: RwLock<Arc<Buffer>>,
    pub(crate) notice: Mutex<Option<Notice>>,
    /// How many lines of a persistent notice PgUp/PgDn have scrolled past (visual audit F4).
    pub(crate) notice_scroll: AtomicUsize,
    /// The two-press exit window (B7). Behind a mutex because `on_key` is the only writer and a
    /// lock is cheaper than making every reader async.
    pub(crate) exit_arm: Mutex<ExitArm>,
    /// The paste detector (B4). `run::on_key` feeds it BEFORE the composer sees the key, and
    /// passes its answer in — the sequencing rule of phase ux1 §2.3, stated once.
    pub(crate) burst: Mutex<draft::PasteBurst>,
    /// The `/` palette. State lives in `bough-plugin-commands`; the shell owns WHEN it is open.
    pub(crate) palette: Mutex<bough_plugin_commands::palette::Palette>,
    /// Whether the open palette is the INLINE autocomplete (a `/`-token mid-draft) rather than
    /// the line-start command palette: inline completes in place and never dispatches.
    pub(crate) palette_inline: AtomicBool,
    /// When the focused agent's wake was first SEEN running, for the elapsed clock (M32). Written
    /// by `note_running` on the idle→running edge, which `draw` calls once a frame.
    pub(crate) running_since: RwLock<Option<chrono::DateTime<chrono::Utc>>>,
    pub(crate) anchored: RwLock<Option<StepId>>,
    /// The last line the shell handed to `ctx.commands`. Observability: the status line shows it,
    /// and it is how "a slash line never became a send" is asserted.
    pub(crate) last_command: Mutex<Option<String>>,
    /// MERGE (note 16). A message submitted BEFORE the roster is up. The composer is painted the
    /// moment the shell mounts, and `residents` raises the agents asynchronously afterwards, so
    /// there is a window — measured at roughly one submit in three on a cold boot — in which
    /// Enter has nobody to send to through no fault of the person typing. The message WAITS here
    /// and the tick sends it as soon as an agent exists; past `PENDING_SEND_TICKS` it is handed
    /// back to the composer with an error, because nothing the user typed is ever destroyed (B3).
    pub(crate) pending_send: Mutex<Option<PendingSend>>,
    /// A program to run OVER the TUI on the run loop's next pass (round 11: a turn's context in
    /// `$EDITOR`). The loop restores the terminal, runs it to completion on the same tty, enters
    /// again and redraws. See [`TuiHandle::run_external`].
    pub(crate) external: Mutex<Option<Vec<String>>>,
    pub(crate) redraw: Notify,
    /// Bumped once per published frame, AFTER `last_frame` is written. An attach session awaits
    /// this to learn there is a new buffer to blit; the counter's value is only an edge.
    pub(crate) frames: tokio::sync::watch::Sender<u64>,
    /// Where out-of-band terminal bytes (OSC52) go when this process has no tty of its own: the
    /// attach row points this at the connected client. `None` means drop them, which is what a
    /// headless test wants.
    pub(crate) byte_sink: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>,
    /// Out-of-band bytes the terminal must keep AGREEING with (a tab title), remembered by key.
    /// A one-shot (OSC52) written while no client is attached is correctly dropped; state written
    /// then would stay dropped forever, so [`TuiHandle::set_byte_sink`] replays these into every
    /// new sink.
    pub(crate) oob_sticky: Mutex<std::collections::BTreeMap<&'static str, Vec<u8>>>,
}

/// A submit waiting for the roster (merge note 16).
#[derive(Clone, Debug, PartialEq)]
pub struct PendingSend {
    pub text: String,
    /// How many ticks it has waited. Bounded by [`PENDING_SEND_TICKS`].
    pub ticks: u32,
}

/// How many ticks a queued submit waits for an agent before it is handed back to the composer.
///
/// A PROTOCOL CONSTANT, not a tunable (§0.2): it bounds a wait the shell owns, and the number that
/// matters is "long enough for a boot, short enough that a tree with no agents at all says so".
/// At the default `tick_ms` of 1000 that is ten seconds; a real roster is up in well under one.
pub const PENDING_SEND_TICKS: u32 = 10;

impl TuiHandle {
    /// Build the shell's state. `agents` and `commands` are what the row injected; a test that
    /// wants neither passes `None` and every path that needs one reports a notice instead.
    pub fn new(
        ctx: Context,
        cfg: Arc<TuiConfig>,
        agents: Option<Arc<AgentsHandle>>,
        commands: Option<Arc<CommandsHandle>>,
        is_tty: bool,
    ) -> Result<TuiHandle, TuiError> {
        let resolved = cfg.backend.resolve(is_tty);
        let be = match resolved {
            Backend::Crossterm => backend::TermBackend::crossterm(),
            _ => backend::TermBackend::headless(cfg.size),
        };
        let mut terminal = Terminal::new(be).map_err(|e| TuiError::Terminal {
            step: "terminal",
            source: e,
        })?;
        let area = terminal.get_frame().area();
        let arm_ms = cfg.exit_arm_ms;
        let burst_ms = cfg.paste_burst_ms;
        let mut composer = Composer::new(&cfg);
        if let Some(c) = commands.as_ref() {
            composer.set_prefix(c.prefix());
        }
        Ok(TuiHandle(Arc::new(TuiInner {
            ctx,
            theme: Theme::of(cfg.theme),
            backend: resolved,
            cfg,
            agents,
            ledger: Mutex::new(None),
            commands,
            terminal: Mutex::new(terminal),
            panes: RwLock::new(Vec::new()),
            rects: RwLock::new(Vec::new()),
            hits: RwLock::new(HashMap::new()),
            reports: RwLock::new(HashMap::new()),
            focused_agent: RwLock::new(None),
            focused_pane: RwLock::new(no_pane()),
            lane_picker: Mutex::new(None),
            composer_focused: AtomicBool::new(true),
            quitting: AtomicBool::new(false),
            focus_chosen: AtomicBool::new(false),
            composer: Mutex::new(composer),
            selection: Mutex::new(None),
            rail_cols: RwLock::new(None),
            rail_drag: AtomicBool::new(false),
            last_frame: RwLock::new(Arc::new(Buffer::empty(area))),
            notice: Mutex::new(None),
            notice_scroll: AtomicUsize::new(0),
            exit_arm: Mutex::new(ExitArm::new(std::time::Duration::from_millis(arm_ms))),
            burst: Mutex::new(draft::PasteBurst::new(std::time::Duration::from_millis(
                burst_ms,
            ))),
            palette: Mutex::new(Default::default()),
            palette_inline: AtomicBool::new(false),
            running_since: RwLock::new(None),
            anchored: RwLock::new(None),
            last_command: Mutex::new(None),
            pending_send: Mutex::new(None),
            external: Mutex::new(None),
            redraw: Notify::new(),
            frames: tokio::sync::watch::Sender::new(0),
            byte_sink: Mutex::new(None),
            oob_sticky: Mutex::new(std::collections::BTreeMap::new()),
        })))
    }

    /// Register a pane. An EFFECT (§0.2): the returned disposer removes the pane from its slot,
    /// drops its hit map and requests a redraw, so a pane row unloading reflows the layout with
    /// no restart (the SWAP gate).
    pub async fn register_pane(
        &self,
        ctx: &Context,
        spec: PaneSpec,
    ) -> Result<EffectHandle, PluginError> {
        let id = spec.id.clone();
        if self.0.panes.read().iter().any(|p| p.info.id == id) {
            return Err(PluginError::new(
                ctx.entry_id().clone(),
                anyhow::anyhow!("a pane with id `{id}` is already registered"),
            ));
        }
        let entry = PaneEntry {
            info: PaneInfo {
                id: id.clone(),
                slot: spec.slot,
                order: spec.order,
                size: spec.size,
                title: spec.title,
                focusable: spec.focusable,
                owner: ctx.entry_id().clone(),
            },
            pane: spec.pane,
        };
        let me = self.clone();
        let for_inverse = self.clone();
        let gone = id.clone();
        ctx.effect(move |e| async move {
            me.0.panes.write().push(entry);
            // A focusable pane takes keyboard focus, so a tree with one pane is usable without a
            // Tab — and the pane it picks is the TRAJECTORY, not whichever row happened to
            // register first (see `default_focus`).
            // Re-derived on EVERY registration until something chooses: rows load in bundle
            // order, so `tui.strip` registers before `tui.focus`, and a rule that only ran while
            // `focused_pane` was still the sentinel would hand the rail every key forever.
            if !me.0.focus_chosen.load(Ordering::SeqCst) {
                let panes: Vec<PaneInfo> =
                    me.0.panes.read().iter().map(|p| p.info.clone()).collect();
                if let Some(first) = default_focus(&panes) {
                    *me.0.focused_pane.write() = first;
                }
            }
            me.redraw();
            e.defer_sync(move || {
                for_inverse.0.panes.write().retain(|p| p.info.id != gone);
                for_inverse.0.hits.write().remove(&gone);
                for_inverse.0.rects.write().retain(|(p, _)| *p != gone);
                if *for_inverse.0.focused_pane.read() == gone {
                    let panes: Vec<PaneInfo> = for_inverse
                        .0
                        .panes
                        .read()
                        .iter()
                        .map(|p| p.info.clone())
                        .collect();
                    let next = default_focus(&panes).unwrap_or_else(no_pane);
                    *for_inverse.0.focused_pane.write() = next;
                }
                for_inverse.redraw();
            });
            Ok(())
        })
        .await
    }

    /// Every live pane, sorted by (slot, order, id). Stable across frames.
    pub fn panes(&self) -> Vec<PaneInfo> {
        pane::sorted(
            &self
                .0
                .panes
                .read()
                .iter()
                .map(|p| p.info.clone())
                .collect::<Vec<_>>(),
        )
    }

    /// The pane objects, in the same order. Internal: `render` needs the object, callers do not.
    pub(crate) fn entries(&self) -> Vec<PaneEntry> {
        let mut v = self.0.panes.read().clone();
        v.sort_by(|a, b| {
            a.info
                .slot
                .cmp(&b.info.slot)
                .then(a.info.order.cmp(&b.info.order))
                .then(a.info.id.cmp(&b.info.id))
        });
        v
    }

    pub(crate) fn entry(&self, id: &PaneId) -> Option<PaneEntry> {
        self.0
            .panes
            .read()
            .iter()
            .find(|p| p.info.id == *id)
            .cloned()
    }

    /// The agent the `Main` slot is showing, if any.
    pub fn focused_agent(&self) -> Option<AgentId> {
        self.0.focused_agent.read().clone()
    }

    /// The default focus, resolved but NOT applied: the lowest-named live agent.
    ///
    /// Explicit `resolve(request) -> Spec` rather than a `??` inside the loop, and name order so
    /// the choice does not depend on the order `residents` happened to raise the roster in.
    pub fn default_agent(&self) -> Option<AgentId> {
        let mut live: Vec<_> = self
            .0
            .agents
            .as_ref()?
            .list()
            .into_iter()
            .filter(|a| !a.is_disposed())
            .collect();
        live.sort_by(|a, b| a.name().as_str().cmp(b.name().as_str()));
        Some(live.first()?.id().clone())
    }

    /// INTEGRATION SEAM (P3-D22). Nothing focused an agent at boot. `focus` only latches what a
    /// pane's click asks for, and `residents` raises the roster with no opinion about the
    /// terminal — so on a fresh boot the composer's Enter found no agent and handed the text
    /// straight back, and the focus pane had no trajectory to draw. The roster is raised
    /// asynchronously AFTER the shell mounts, so this cannot be done once at startup; the loop
    /// asks on every tick and the first tick that finds an agent adopts it.
    ///
    /// It goes through the full `focus` path on purpose: the panes learn their target from the
    /// `tui/focus` event, so latching the id alone would leave the focus pane blank.
    pub async fn adopt_default_agent(&self) {
        if self.focused_agent().is_some() {
            return;
        }
        let Some(agent) = self.default_agent() else {
            return;
        };
        self.focus(FocusRequest {
            agent: Some(agent),
            ..Default::default()
        })
        .await;
    }

    /// The pane holding keyboard focus.
    pub fn focused_pane(&self) -> PaneId {
        self.0.focused_pane.read().clone()
    }

    /// Whether the composer holds keyboard focus.
    pub fn composer_focused(&self) -> bool {
        self.0.composer_focused.load(Ordering::SeqCst)
    }

    /// Moves focus and emits `tui/focus`. `step` is a request the focus pane consumes.
    pub async fn focus(&self, req: FocusRequest) {
        if let Some(agent) = req.agent.clone() {
            *self.0.focused_agent.write() = Some(agent);
        }
        if let Some(step) = req.step.clone() {
            *self.0.anchored.write() = Some(step);
        }
        if let Some(p) = req.pane.clone() {
            self.set_focus_pane(p);
        }
        self.0.ctx.emit::<TuiFocusEvent>(req.clone());
        // Every pane hears the request: the strip repaints its selection, the focus pane scrolls.
        for e in self.entries() {
            let cx = self.pane_cx();
            let _ = e.pane.handle(PaneEvent::Focus(req.clone()), cx).await;
        }
        self.redraw();
    }

    /// Move keyboard focus to one pane.
    pub async fn focus_pane(&self, pane: PaneId) {
        let previous = self.focused_pane();
        if previous == pane {
            // Already the focused pane, but the COMPOSER may have had the keyboard: taking it back
            // is the whole point of the click that got here.
            self.set_focus_pane(pane);
            self.redraw();
            return;
        }
        self.set_focus_pane(pane.clone());
        if let Some(e) = self.entry(&previous) {
            let cx = self.pane_cx();
            let _ = e.pane.handle(PaneEvent::FocusChanged(false), cx).await;
        }
        if let Some(e) = self.entry(&pane) {
            let cx = self.pane_cx();
            let _ = e.pane.handle(PaneEvent::FocusChanged(true), cx).await;
        }
        self.redraw();
    }

    pub(crate) fn set_focus_pane(&self, pane: PaneId) {
        self.0.focus_chosen.store(true, Ordering::SeqCst);
        *self.0.focused_pane.write() = pane;
        self.0.composer_focused.store(false, Ordering::SeqCst);
    }

    /// Give the composer keyboard focus.
    /// The composer takes the keyboard back, TELLING the pane that had it. `focus_composer` alone
    /// cannot: it is sync, and a pane's `FocusChanged` handler is async — which is why the search
    /// field kept its stale query after Esc (minor 30).
    pub async fn give_keyboard_to_composer(&self) {
        if self.composer_focused() {
            return;
        }
        let previous = self.focused_pane();
        self.focus_composer();
        if let Some(e) = self.entry(&previous) {
            let cx = self.pane_cx();
            let _ = e.pane.handle(PaneEvent::FocusChanged(false), cx).await;
        }
        self.redraw();
    }

    /// Hand the shell the ledger it injected. Called once from `apply`; a test that drives the
    /// shell without a ledger leaves it unset and `/agents` reports the live status alone.
    pub fn set_ledger(&self, ledger: Arc<bough_plugin_ledger::LedgerHandle>) {
        *self.0.ledger.lock() = Some(ledger);
    }

    pub fn focus_composer(&self) {
        self.0.composer_focused.store(true, Ordering::SeqCst);
        self.redraw();
    }

    /// Coalesced: many calls in one frame budget cost one frame.
    pub fn redraw(&self) {
        self.0.redraw.notify_one();
    }

    /// One-line transient message in [`Slot::Status`]. Kind [`NoticeKind::Info`].
    pub fn notify(&self, text: impl Into<String>) {
        self.notify_kind(text, NoticeKind::Info);
    }

    /// The current notice's text, if any.
    pub fn notice(&self) -> Option<String> {
        self.0.notice.lock().as_ref().map(|n| n.text.clone())
    }

    /// The current notice with its role, TTL not yet applied.
    pub fn notice_raw(&self) -> Option<Notice> {
        self.0.notice.lock().clone()
    }

    /// Drop the notice (any key does this for an error notice, which has no TTL).
    pub fn clear_notice(&self) {
        self.0.notice_scroll.store(0, Ordering::Relaxed);
        *self.0.notice.lock() = None;
    }

    /// The last line handed to `ctx.commands`. Never a message that was sent to an agent (V5).
    pub fn last_command(&self) -> Option<String> {
        self.0.last_command.lock().clone()
    }

    /// OSC52 to the terminal + `arboard` when configured. Never fails the caller (P3-D7).
    pub async fn copy(&self, text: &str) -> CopyOutcome {
        let mut out: Vec<u8> = Vec::new();
        let outcome = clip::copy(text, &self.0.cfg, &mut out).await;
        self.write_oob(out);
        // The flash is ALWAYS a line (WP-7): a silent success is the audit's "did it copy?".
        let (text, kind) = outcome.flash();
        self.notify_kind(text, kind);
        outcome
    }

    /// Out-of-band bytes to the terminal the user is looking at: our own tty when this process
    /// has one, the attached client's otherwise (its terminal is where the selection was dragged,
    /// where the tab lives — so its emulator is the one meant). Escape sequences that address the
    /// emulator rather than the frame (OSC52 copy, the tab title) ride this; a headless test's
    /// bytes are dropped, which is what it wants.
    pub fn write_oob(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        if self.0.backend == Backend::Crossterm {
            use std::io::Write;
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(&bytes);
            let _ = stdout.flush();
        } else if let Some(sink) = self.0.byte_sink.lock().as_ref() {
            let _ = sink.send(bytes);
        }
    }

    /// Out-of-band bytes the terminal must keep agreeing with (a tab title): written now, and
    /// remembered under `key` so [`TuiHandle::set_byte_sink`] can replay them to a client that
    /// attaches later — the resident mounts its rows long before anyone is connected to hear them.
    pub fn set_oob_sticky(&self, key: &'static str, bytes: Vec<u8>) {
        self.0.oob_sticky.lock().insert(key, bytes.clone());
        self.write_oob(bytes);
    }

    /// Forget a sticky entry. The unloading row writes its own parting bytes first; forgetting
    /// only stops the replay, so a reload never re-asserts a predecessor's state (§0.2).
    pub fn clear_oob_sticky(&self, key: &'static str) {
        self.0.oob_sticky.lock().remove(key);
    }

    /// The whole terminal.
    pub fn size(&self) -> Rect {
        self.0.terminal.lock().get_frame().area()
    }

    /// Resize the surface. On crossterm the terminal reports its own size and this only forces the
    /// relayout; on the headless backend it IS the size, which is how a resize is exercised
    /// without a terminal.
    pub fn resize(&self, width: u16, height: u16) {
        let mut terminal = self.0.terminal.lock();
        if let backend::TermBackend::Headless(b) = terminal.backend_mut() {
            b.resize(width.max(1), height.max(1));
        }
        let _ = terminal.resize(Rect::new(0, 0, width.max(1), height.max(1)));
        drop(terminal);
        self.redraw();
    }

    /// Which backend the shell resolved `Backend::Auto` to (P3-D2).
    pub fn backend(&self) -> Backend {
        self.0.backend
    }

    /// The last rendered buffer. The selection reads from it; tests assert against it.
    pub fn last_frame(&self) -> Arc<Buffer> {
        self.0.last_frame.read().clone()
    }

    /// A receiver that observes every published frame ([`TuiInner::frames`]). `changed()` resolves
    /// once per `draw` that ran to completion; the value is a counter and carries no meaning
    /// beyond the edge.
    pub fn frames(&self) -> tokio::sync::watch::Receiver<u64> {
        self.0.frames.subscribe()
    }

    /// Point the out-of-band byte path (OSC52) at a sink, or take it away. The attach row is the
    /// one caller; everything else leaves it `None`. A new sink is a terminal that saw NONE of
    /// the sticky state (its tab was titled by its shell, not by us), so every sticky entry is
    /// replayed into it before anything else rides the path.
    pub fn set_byte_sink(&self, sink: Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>) {
        let replay: Vec<Vec<u8>> = if sink.is_some() {
            self.0.oob_sticky.lock().values().cloned().collect()
        } else {
            Vec::new()
        };
        *self.0.byte_sink.lock() = sink;
        for bytes in replay {
            self.write_oob(bytes);
        }
    }

    /// Whether this composition captures the mouse — what an attach client must mirror on its own
    /// terminal, learned over the wire rather than from a config it does not have.
    pub fn mouse(&self) -> bool {
        self.0.cfg.mouse
    }

    /// Whether this composition pushes the kitty keyboard-enhancement flags — the attach client
    /// mirrors this on its own terminal, same as [`TuiHandle::mouse`].
    pub fn keyboard_enhancement(&self) -> bool {
        self.0.cfg.keyboard_enhancement
    }

    /// The current drag, if one is in progress or has just finished.
    pub fn selection(&self) -> Option<Selection> {
        *self.0.selection.lock()
    }

    /// The step the focus pane was last asked to anchor on, if any.
    pub fn anchored_step(&self) -> Option<StepId> {
        self.0.anchored.read().clone()
    }

    /// The composer's text.
    pub fn composer_text(&self) -> String {
        self.0.composer.lock().text()
    }

    /// Put text in the composer.
    pub fn set_composer_text(&self, text: &str) {
        self.0.composer.lock().set_text(text);
        self.redraw();
    }

    /// QUEUE a submit that arrived before the roster was up (merge note 16). Returns `false` when
    /// something is already queued — the caller then hands the text back to the composer rather
    /// than losing whichever of the two it overwrote.
    pub fn queue_send(&self, text: &str) -> bool {
        let mut slot = self.0.pending_send.lock();
        if slot.is_some() {
            return false;
        }
        *slot = Some(PendingSend {
            text: text.to_string(),
            ticks: 0,
        });
        true
    }

    /// The submit waiting for an agent, if any. The status of the queue, for a test and for the
    /// tick that drains it.
    pub fn pending_send(&self) -> Option<PendingSend> {
        self.0.pending_send.lock().clone()
    }

    /// Take the queued submit out. `None` leaves the slot alone.
    pub(crate) fn take_pending_send(&self) -> Option<PendingSend> {
        self.0.pending_send.lock().take()
    }

    /// One more tick of waiting. Returns the count AFTER the bump.
    pub(crate) fn bump_pending_send(&self) -> u32 {
        let mut slot = self.0.pending_send.lock();
        match slot.as_mut() {
            Some(p) => {
                p.ticks += 1;
                p.ticks
            }
            None => 0,
        }
    }

    /// The rectangle a pane was given by the last layout.
    pub fn rect_of(&self, id: &PaneId) -> Option<Rect> {
        self.0
            .rects
            .read()
            .iter()
            .find(|(p, _)| p == id)
            .map(|(_, r)| *r)
    }

    /// The pane under a cell, by the last layout.
    pub fn pane_at(&self, col: u16, row: u16) -> Option<PaneId> {
        self.0
            .rects
            .read()
            .iter()
            .rev()
            .find(|(_, r)| {
                col >= r.x
                    && col < r.x.saturating_add(r.width)
                    && row >= r.y
                    && row < r.y.saturating_add(r.height)
            })
            .map(|(p, _)| p.clone())
    }

    /// The hit a pane recorded for a cell in the last frame.
    pub fn hit_at(&self, pane: &PaneId, col: u16, row: u16) -> Option<HitId> {
        self.0.hits.read().get(pane).and_then(|m| m.at(col, row))
    }

    /// The focused lane's pending question from the transcript pane's last report.
    pub fn owed(&self) -> bool {
        self.transcript_pane()
            .and_then(|t| self.0.reports.read().get(&t).and_then(|r| r.owed))
            .unwrap_or(false)
    }

    /// Whether quit has been asked for. A pane that reads the ledger on an event checks this
    /// first: no reads once shutdown begins.
    /// Run `argv` over the TUI: the run loop leaves the alt screen, runs it to completion on the
    /// same tty, then enters again and redraws (round 11). The request is queued; nothing
    /// happens until the loop's next pass, which `redraw` wakes. A later request before that
    /// pass replaces an earlier one.
    pub fn run_external(&self, argv: Vec<String>) {
        if argv.is_empty() {
            return;
        }
        *self.0.external.lock() = Some(argv);
        self.redraw();
    }

    pub(crate) fn take_external(&self) -> Option<Vec<String>> {
        self.0.external.lock().take()
    }

    pub fn quitting(&self) -> bool {
        self.0.quitting.load(Ordering::SeqCst)
    }

    /// The lanes the `to:` picker lists, by name: every live agent, sorted.
    pub fn lanes(&self) -> Vec<Agent> {
        let mut lanes: Vec<Agent> = self.0.agents.as_ref().map(|a| a.list()).unwrap_or_default();
        lanes.sort_by(|a, b| a.name().as_str().cmp(b.name().as_str()));
        lanes
    }

    /// The `to:` picker's state: `Some(selected)` while open.
    pub fn lane_picker(&self) -> Option<usize> {
        *self.0.lane_picker.lock()
    }

    pub fn set_lane_picker(&self, state: Option<usize>) {
        *self.0.lane_picker.lock() = state;
        self.redraw();
    }

    /// The commands registry this shell dispatches through, when the row injected one.
    pub fn commands(&self) -> Option<Arc<CommandsHandle>> {
        self.0.commands.clone()
    }

    /// The live handle of the focused agent, when the roster has one.
    pub fn agent(&self) -> Option<Agent> {
        let id = self.focused_agent()?;
        self.0.agents.as_ref()?.get(&id)
    }

    /// What a pane's `handle` runs against.
    pub(crate) fn pane_cx(&self) -> PaneCx {
        PaneCx {
            tui: self.clone(),
            agent: self.agent(),
            at: chrono::Utc::now(),
        }
    }

    /// The read-only view a render is handed.
    pub(crate) fn view(
        &self,
        pane: &PaneId,
        now: chrono::DateTime<chrono::Utc>,
        size: Rect,
    ) -> ShellView {
        ShellView {
            focused_agent: self.focused_agent(),
            focused_pane: self.focused_pane(),
            is_focused: self.focused_pane() == *pane && !self.composer_focused(),
            selection: self.selection().map(|s| s.rect()),
            size,
            theme: self.0.theme,
            now,
            composer_focused: self.composer_focused(),
            composer_nonempty: !self.0.composer.lock().text().is_empty(),
            // What the panes REPORTED on the last frame (`RenderCx::report_rows`). A pane's
            // roving row focus and the transcript's follow state live inside the pane — the
            // shell cannot read them — so the honest shape is a report, not a guess. A pane
            // that reports nothing leaves the documented defaults, which is what "a pane that
            // ignores them renders exactly as before" means (§2.12).
            row_focus: self.0.reports.read().get(pane).and_then(|r| r.row_focus),
            following: self
                .transcript_pane()
                .and_then(|t| self.0.reports.read().get(&t).map(|r| r.following))
                .unwrap_or(true),
            measure_cols: self.0.cfg.measure_cols,
            focused_name: self.agent().map(|a| a.name().to_string()),
            running: self.running(),
            owed_question: self.owed(),
            rail_collapsed: {
                // Last frame's layout, the same source `status_top` reads: a Strip pane handed
                // zero columns is a collapsed rail; no Strip pane at all is no rail.
                let rects = self.0.rects.read();
                !self.panes().iter().any(|p| {
                    p.slot == pane::Slot::Strip
                        && rects.iter().any(|(id, r)| *id == p.id && r.width > 0)
                })
            },
            notice_pinned: self.notice_raw().is_some_and(|n| n.ttl.is_none()),
        }
    }

    // -----------------------------------------------------------------------------------
    // phase ux1 §2.1/§2.4: what the keymap, the status line and the exit machine read
    // -----------------------------------------------------------------------------------

    /// The pane the focus-independent scroll keys drive: [`TuiConfig::transcript_pane`], matched
    /// EXACTLY. `None` when no such pane is registered (B2).
    pub fn transcript_pane(&self) -> Option<PaneId> {
        let want = self.0.cfg.transcript_pane.as_str();
        self.panes()
            .into_iter()
            .find(|p| p.id.as_str() == want)
            .map(|p| p.id)
    }

    /// Whether the focused agent has a wake open right now. The status line's spinner and the
    /// `esc to interrupt` hint read this; `keymap` reads it to decide what Esc means.
    pub fn running(&self) -> bool {
        matches!(
            self.agent().map(|a| a.status()),
            Some(bough_plugin_agents::Status::Running)
        )
    }

    /// When the running wake started, for the elapsed clock (M32).
    pub fn running_since(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        *self.0.running_since.read()
    }

    /// Latch the idle→running edge. Called once a frame by `draw`, which is the only place with a
    /// `now` that is not a clock read inside a pure function.
    pub(crate) fn note_running(&self, now: chrono::DateTime<chrono::Utc>) {
        let running = self.running();
        let mut held = self.0.running_since.write();
        match (running, *held) {
            (true, None) => *held = Some(now),
            (false, Some(_)) => *held = None,
            _ => {}
        }
    }

    /// `Ctrl+C` has been pressed once and the window has not lapsed (B7).
    pub fn exit_armed(&self) -> bool {
        self.0.exit_arm.lock().is_armed(chrono::Utc::now())
    }

    /// One `Ctrl+C` while idle: arm, or exit if the window is still open (B7).
    pub fn exit_step(&self, now: chrono::DateTime<chrono::Utc>) -> ExitStep {
        self.0.exit_arm.lock().press(now)
    }

    /// Any other key disarms: `press Ctrl+C again to exit` must not survive a change of mind.
    pub fn disarm_exit(&self) {
        self.0.exit_arm.lock().disarm();
    }

    /// A transient notice with a ROLE, so the theme can colour an error like an error (M22).
    pub fn notify_kind(&self, text: impl Into<String>, kind: NoticeKind) {
        // An ERROR has no TTL: it waits for the next key, because a message that fades before it
        // is read is a message that was never shown (M22).
        let ttl = match kind {
            NoticeKind::Error | NoticeKind::Command => None,
            NoticeKind::Copied => Some(std::time::Duration::from_millis(self.0.cfg.flash_ms)),
            _ => Some(std::time::Duration::from_millis(self.0.cfg.notice_ms)),
        };
        *self.0.notice.lock() = Some(Notice {
            text: text.into(),
            kind,
            at: chrono::Utc::now(),
            ttl,
        });
        self.0.notice_scroll.store(0, Ordering::Relaxed);
        self.redraw();
    }

    /// Scroll a persistent notice by `delta` lines (PgUp/PgDn while `/help` is up). Returns
    /// false when there is no such notice, so the key falls through to the panes.
    pub fn scroll_notice(&self, delta: i32, max: usize) -> bool {
        if !matches!(self.notice_raw().map(|n| n.ttl), Some(None)) {
            return false;
        }
        let cur = self.0.notice_scroll.load(Ordering::Relaxed) as i64;
        let next = (cur + delta as i64).clamp(0, max as i64) as usize;
        self.0.notice_scroll.store(next, Ordering::Relaxed);
        self.redraw();
        true
    }

    /// The notice that is still live at `now`, TTL applied.
    pub fn notice_now(&self, now: chrono::DateTime<chrono::Utc>) -> Option<Notice> {
        let held = self.0.notice.lock().clone()?;
        match held.ttl {
            None => Some(held),
            Some(ttl) => match (now - held.at).to_std() {
                Ok(age) if age >= ttl => None,
                _ => Some(held),
            },
        }
    }

    /// Whether the `/` palette is open (M12: an overlay Esc dismisses).
    pub fn palette_open(&self) -> bool {
        self.0.palette.lock().open
    }

    /// Whether the open palette is the inline autocomplete (see `palette_inline` on the inner).
    pub fn palette_inline(&self) -> bool {
        self.0
            .palette_inline
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Mark the open palette inline (completes in place) or command (dispatches).
    pub fn set_palette_inline(&self, inline: bool) {
        self.0
            .palette_inline
            .store(inline, std::sync::atomic::Ordering::Relaxed);
    }

    /// Open the palette on a query, or close it. The shell owns WHEN; `commands` owns WHAT.
    pub fn set_palette(&self, open: bool, query: &str) {
        let mut p = self.0.palette.lock();
        if open && !p.open {
            p.selected = 0;
        }
        p.open = open;
        p.query = query.to_string();
        drop(p);
        self.redraw();
    }

    /// Whether ANY overlay is up: today the palette, or a notice that waits for a key.
    pub fn overlay_open(&self) -> bool {
        self.palette_open() || matches!(self.notice_raw().map(|n| n.ttl), Some(None))
    }

    /// The keymap's whole input, read ONCE (phase ux1 §2.1).
    pub fn key_context(&self) -> KeyContext {
        KeyContext {
            focus_is_composer: self.composer_focused(),
            draft_is_empty: self.0.composer.lock().is_empty(),
            running: self.running(),
            overlay_open: self.overlay_open(),
            palette_open: self.palette_open(),
            exit_armed: self.exit_armed(),
        }
    }

    /// Quit, with the one-line farewell printed AFTER the terminal is restored (B8).
    pub fn quit_with(&self, code: u8, farewell: impl Into<String>) {
        term::set_farewell(farewell.into());
        self.quit(code);
    }

    /// Ask the process to end. Delegates to `Kernel::request_exit` (P2-D23): the launcher still
    /// owns teardown, and teardown is what restores the terminal.
    pub fn quit(&self, code: u8) {
        self.0.quitting.store(true, Ordering::SeqCst);
        match self.0.ctx.kernel() {
            Some(k) => k.request_exit(code),
            // No kernel means no launcher to ask; restoring is then the only honest thing left.
            None => term::restore_now(),
        }
    }
}

/// The editor as argv: `$VISUAL`, else `$EDITOR`, else `vi` — split on whitespace so
/// `EDITOR="code --wait"` works.
pub fn editor_argv() -> Vec<String> {
    for var in ["VISUAL", "EDITOR"] {
        if let Ok(v) = std::env::var(var) {
            let argv: Vec<String> = v.split_whitespace().map(str::to_string).collect();
            if !argv.is_empty() {
                return argv;
            }
        }
    }
    vec!["vi".to_string()]
}

/// Everything the shell can go wrong as before there is a screen to say it on.
#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal setup failed at `{step}`: {source}")]
    Terminal {
        step: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Failed(String),
}

/// The row's config. Every deployment-varying value is here; nothing is a `DEFAULT_` constant.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TuiConfig {
    /// `auto` (default): crossterm when stdout is a TTY, else the headless TestBackend, so
    /// `--check` and CI can mount the tui profile without a terminal (P3-D2).
    pub backend: Backend,
    /// Size of the headless backend. Ignored by crossterm.
    pub size: [u16; 2],
    /// Redraw coalescing budget.
    pub frame_ms: u64,
    /// Relative-time refresh; also the [`PaneEvent::Tick`] cadence.
    pub tick_ms: u64,
    pub theme: ThemeName,
    pub mouse: bool,
    pub osc52: bool,
    /// Best-effort `arboard` in addition to OSC52.
    pub clipboard: bool,
    pub composer_max_lines: u16,
    /// The pane `Ctrl+F` gives the keyboard to. A pane id, matched EXACTLY: the binding used to
    /// substring-match "search", so any future pane whose id contained the word stole it and a
    /// rename broke it silently into the "no search pane" notice.
    #[serde(default = "default_search_pane")]
    pub search_pane: String,
    /// The shell's OWN fallback page size, in lines, for a pane that ignores PageUp/PageDown.
    /// A pane with a `page_lines` of its own (`tui.focus`) honours that first.
    #[serde(default = "default_page_lines")]
    pub page_lines: u16,
    /// Lines per wheel notch.
    #[serde(default = "default_wheel_lines")]
    pub wheel_lines: u16,
    /// How many rows a notice may borrow above the composer before it is truncated.
    #[serde(default = "default_notice_lines")]
    pub notice_max_lines: u16,
    /// The pane the focus-independent scroll keys drive (phase ux1 §2.2, B2). A pane id, matched
    /// EXACTLY — the same lesson `search_pane` already carries.
    #[serde(default = "default_transcript_pane")]
    pub transcript_pane: String,
    /// The prose measure cap, in columns (M13). A 200-column terminal gets a 90-column paragraph.
    #[serde(default = "default_measure_cols")]
    pub measure_cols: u16,
    /// Blank columns between the rail and the transcript, owned by neither (M9).
    #[serde(default = "default_gutter")]
    pub gutter: u16,
    /// Heavy rules between the panels (Andrey, 2026-08-28): `┃` down the gutter between the rail
    /// and the conversation, `━` across the row above the bottom bands. The rule row is taken
    /// from the conversation.
    #[serde(default = "default_borders")]
    pub borders: bool,
    /// How long a `Ctrl+C` stays armed before it re-arms (B7).
    #[serde(default = "default_exit_arm_ms")]
    pub exit_arm_ms: u64,
    /// Two keys closer together than this are a PASTE, not typing (B4).
    #[serde(default = "default_paste_burst_ms")]
    pub paste_burst_ms: u64,
    /// How many sent messages `Up` can recall (M20).
    #[serde(default = "default_history_cap")]
    pub history_cap: usize,
    /// How long a transient notice stays up. An error notice has no TTL and waits for a key.
    #[serde(default = "default_notice_ms")]
    pub notice_ms: u64,
    /// How long the copy flash and its selection stay painted (M21).
    #[serde(default = "default_flash_ms")]
    pub flash_ms: u64,
    /// Push the kitty keyboard-enhancement flags (round 10: Shift+Enter as a newline) on a
    /// terminal that supports them. `false` leaves the terminal in legacy key reporting — the
    /// switch for a terminal that misreports keys under the protocol.
    #[serde(default = "default_true")]
    pub keyboard_enhancement: bool,
}

fn default_true() -> bool {
    true
}

fn default_search_pane() -> String {
    "tui.search".to_string()
}
fn default_page_lines() -> u16 {
    10
}
fn default_wheel_lines() -> u16 {
    3
}
fn default_transcript_pane() -> String {
    "tui.focus".to_string()
}
fn default_measure_cols() -> u16 {
    90
}
fn default_gutter() -> u16 {
    1
}
fn default_borders() -> bool {
    true
}
fn default_exit_arm_ms() -> u64 {
    3000
}
fn default_paste_burst_ms() -> u64 {
    20
}
fn default_history_cap() -> usize {
    200
}
fn default_notice_ms() -> u64 {
    6000
}
fn default_flash_ms() -> u64 {
    900
}
fn default_notice_lines() -> u16 {
    // `/help` lists every registered command, and Phase 5 added seven. Eight rows cut the list
    // off mid-alphabet; the band is bounded by the rows above the composer anyway, so the cap's
    // job is only to stop an enormous notice from swallowing the screen.
    24
}

/// The config every test in this crate starts from: headless, deterministic, no clipboard.
#[doc(hidden)]
pub fn test_config() -> TuiConfig {
    TuiConfig {
        backend: Backend::Headless,
        size: [80, 24],
        frame_ms: 1,
        tick_ms: 1000,
        theme: ThemeName::Dark,
        mouse: true,
        osc52: true,
        clipboard: false,
        composer_max_lines: 8,
        search_pane: default_search_pane(),
        page_lines: default_page_lines(),
        wheel_lines: default_wheel_lines(),
        notice_max_lines: default_notice_lines(),
        transcript_pane: default_transcript_pane(),
        measure_cols: default_measure_cols(),
        gutter: default_gutter(),
        borders: default_borders(),
        exit_arm_ms: default_exit_arm_ms(),
        paste_burst_ms: default_paste_burst_ms(),
        history_cap: default_history_cap(),
        notice_ms: default_notice_ms(),
        flash_ms: default_flash_ms(),
        keyboard_enhancement: true,
    }
}

/// The row.
pub struct TuiShellPlugin;

#[async_trait::async_trait]
impl Plugin for TuiShellPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = TuiConfig;

    fn inject() -> Inject {
        Inject::required(["agents", "ledger", "commands"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        // These used to be `.max(1)`d at the use site, which turns a nonsense value into a silent
        // clamp instead of the loud load failure §0.2 asks for.
        if cfg.frame_ms == 0 {
            return reject("frame_ms must be > 0".to_string());
        }
        if cfg.tick_ms == 0 {
            return reject("tick_ms must be > 0".to_string());
        }
        if cfg.composer_max_lines == 0 {
            return reject("composer_max_lines must be > 0".to_string());
        }
        if cfg.page_lines == 0 {
            return reject("page_lines must be > 0".to_string());
        }
        if cfg.wheel_lines == 0 {
            return reject("wheel_lines must be > 0".to_string());
        }
        if cfg.notice_max_lines == 0 {
            return reject("notice_max_lines must be > 0".to_string());
        }
        if cfg.search_pane.trim().is_empty() {
            return reject("search_pane must name a pane id".to_string());
        }
        if cfg.size[0] == 0 || cfg.size[1] == 0 {
            return reject("size must be a non-zero width and height".to_string());
        }
        // The eight fields phase ux1 added. Each one of these silently degraded a behaviour
        // rather than failing the load: `exit_arm_ms: 0` makes `ExitArm::is_armed` never true,
        // so the two-press exit is simply gone; `history_cap: 0` was clamped by a `.max(1)` at
        // the use site, the exact anti-pattern this block exists to replace; an empty
        // `transcript_pane` degrades every PageUp to whatever holds the keyboard (B2).
        if cfg.transcript_pane.trim().is_empty() {
            return reject("transcript_pane must name a pane id".to_string());
        }
        // `measure_cols: 0` is legal: NO cap, prose wraps at the pane's full width.
        if cfg.exit_arm_ms == 0 {
            return reject("exit_arm_ms must be > 0".to_string());
        }
        if cfg.paste_burst_ms == 0 {
            return reject("paste_burst_ms must be > 0".to_string());
        }
        if cfg.history_cap == 0 {
            return reject("history_cap must be > 0".to_string());
        }
        if cfg.notice_ms == 0 {
            return reject("notice_ms must be > 0".to_string());
        }
        if cfg.flash_ms == 0 {
            return reject("flash_ms must be > 0".to_string());
        }
        // `gutter` is the one field a zero is MEANINGFUL for: no blank column between the rail
        // and the transcript. It is bounded above instead, so a patch cannot eat the transcript.
        if cfg.gutter > 16 {
            return reject("gutter must be <= 16 columns".to_string());
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let err = |e: anyhow::Error| PluginError::new(ctx.entry_id().clone(), e);
        let agents = ctx
            .get::<Agents>()
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
        let commands = ctx
            .get::<Commands>()
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;

        let is_tty = term::stdout_is_tty();
        let resolved = cfg.backend.resolve(is_tty);

        // The panic hook goes in BEFORE the terminal is entered, so a panic between the two still
        // finds a hook, and its inverse is an effect: unloading the row puts the old hook back.
        let restore_hook = term::install_panic_hook();
        let hook_cell = Mutex::new(Some(restore_hook));
        ctx.effect(move |e| async move {
            e.defer_sync(move || {
                if let Some(f) = hook_cell.lock().take() {
                    f();
                }
            });
            Ok(())
        })
        .await?;

        if resolved == Backend::Crossterm {
            let guard = TerminalGuard::enter(&cfg).map_err(|e| err(e.into()))?;
            let cell = Mutex::new(Some(guard));
            ctx.effect(move |e| async move {
                e.defer_sync(move || {
                    // Dropping the guard leaves exactly what `enter` set (V8).
                    drop(cell.lock().take());
                });
                Ok(())
            })
            .await?;
        }

        let tui = TuiHandle::new(
            ctx.clone(),
            cfg.clone(),
            Some(agents),
            Some(commands),
            is_tty,
        )
        .map_err(|e| err(e.into()))?;
        if let Ok(ledger) = ctx.get::<bough_plugin_ledger::Ledger>() {
            tui.set_ledger(ledger);
        }

        ctx.provide::<Tui>(tui.clone())
            .await
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;

        builtins::register(&ctx, &tui).await?;

        // M15: the reload result reaches the SCREEN, in the log's own words. The listener is an
        // EFFECT OF THIS ROW (§0.1 item 2, §0.2): it holds the handle this row provides, it is
        // rebuilt whenever this row reloads — which a saved patch file, the very event being
        // reported, can cause — and disabling `tui` by patch takes it away. It used to live in
        // the launcher, holding an Arc captured once at boot, so M15 stopped reaching the screen
        // the first time the `tui` row reloaded, with nothing failing.
        let notice_tui = tui.clone();
        ctx.on::<bough_kernel::ConfigReloadEvent, _, _>(move |what: bough_kernel::ConfigReload| {
            let tui = notice_tui.clone();
            async move {
                let kind = if what.is_rejection() {
                    NoticeKind::Error
                } else {
                    NoticeKind::Config
                };
                tui.notify_kind(what.line(), kind);
            }
        })
        .await
        .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;

        // The loop is an effect: disposing the row halts it at its next checkpoint.
        let (loop_ctx, loop_tui, loop_cfg) = (ctx.clone(), tui.clone(), cfg.clone());
        ctx.effect_spawn(move |e| async move {
            // A panic inside a pane's render unwinds HERE. The panic hook has already restored the
            // terminal; asking the kernel to exit 101 is what makes the launcher tear the tree
            // down instead of leaving a live process with a dead screen (V8).
            let kernel = loop_ctx.kernel();
            let body = std::panic::AssertUnwindSafe(run::run(loop_ctx, loop_tui, loop_cfg, e));
            if futures::FutureExt::catch_unwind(body).await.is_err() {
                match kernel {
                    Some(k) => {
                        tracing::error!("tui: a pane panicked; requesting exit 101");
                        k.request_exit(101);
                    }
                    None => tracing::error!("tui: a pane panicked and there is no kernel handle"),
                }
            }
            Ok(())
        });
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(TuiShellPlugin);
