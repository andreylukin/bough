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
pub mod events;
pub mod invariant;
pub mod pane;
pub mod run;
pub mod select;
pub mod term;
pub mod theme;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
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
    pub(crate) commands: Option<Arc<CommandsHandle>>,
    pub(crate) terminal: Mutex<Terminal<backend::TermBackend>>,
    pub(crate) panes: RwLock<Vec<PaneEntry>>,
    pub(crate) rects: RwLock<Vec<(PaneId, Rect)>>,
    pub(crate) hits: RwLock<HashMap<PaneId, HitMap>>,
    pub(crate) focused_agent: RwLock<Option<AgentId>>,
    pub(crate) focused_pane: RwLock<PaneId>,
    pub(crate) composer_focused: AtomicBool,
    /// Set once anything CHOSE a pane (a click, Tab, a `FocusRequest`). Until then keyboard focus
    /// is only a default, and every new registration re-derives it — see `default_focus`.
    pub(crate) focus_chosen: AtomicBool,
    pub(crate) composer: Mutex<Composer>,
    pub(crate) selection: Mutex<Option<Selection>>,
    pub(crate) last_frame: RwLock<Arc<Buffer>>,
    pub(crate) notice: Mutex<Option<String>>,
    pub(crate) anchored: RwLock<Option<StepId>>,
    /// The last line the shell handed to `ctx.commands`. Observability: the status line shows it,
    /// and it is how "a slash line never became a send" is asserted.
    pub(crate) last_command: Mutex<Option<String>>,
    pub(crate) redraw: Notify,
}

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
            commands,
            terminal: Mutex::new(terminal),
            panes: RwLock::new(Vec::new()),
            rects: RwLock::new(Vec::new()),
            hits: RwLock::new(HashMap::new()),
            focused_agent: RwLock::new(None),
            focused_pane: RwLock::new(no_pane()),
            composer_focused: AtomicBool::new(true),
            focus_chosen: AtomicBool::new(false),
            composer: Mutex::new(composer),
            selection: Mutex::new(None),
            last_frame: RwLock::new(Arc::new(Buffer::empty(area))),
            notice: Mutex::new(None),
            anchored: RwLock::new(None),
            last_command: Mutex::new(None),
            redraw: Notify::new(),
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
    pub fn focus_composer(&self) {
        self.0.composer_focused.store(true, Ordering::SeqCst);
        self.redraw();
    }

    /// Coalesced: many calls in one frame budget cost one frame.
    pub fn redraw(&self) {
        self.0.redraw.notify_one();
    }

    /// One-line transient message in [`Slot::Status`].
    pub fn notify(&self, text: impl Into<String>) {
        *self.0.notice.lock() = Some(text.into());
        self.redraw();
    }

    /// The current notice, if any.
    pub fn notice(&self) -> Option<String> {
        self.0.notice.lock().clone()
    }

    /// The last line handed to `ctx.commands`. Never a message that was sent to an agent (V5).
    pub fn last_command(&self) -> Option<String> {
        self.0.last_command.lock().clone()
    }

    /// OSC52 to the terminal + `arboard` when configured. Never fails the caller (P3-D7).
    pub async fn copy(&self, text: &str) -> CopyOutcome {
        let mut out: Vec<u8> = Vec::new();
        let outcome = clip::copy(text, &self.0.cfg, &mut out).await;
        if !out.is_empty() && self.0.backend == Backend::Crossterm {
            use std::io::Write;
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(&out);
            let _ = stdout.flush();
        }
        match outcome.notice() {
            Some(n) => self.notify(n),
            None => self.notify(format!("copied {} chars", text.chars().count())),
        }
        outcome
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
        }
    }

    /// Ask the process to end. Delegates to `Kernel::request_exit` (P2-D23): the launcher still
    /// owns teardown, and teardown is what restores the terminal.
    pub fn quit(&self, code: u8) {
        match self.0.ctx.kernel() {
            Some(k) => k.request_exit(code),
            // No kernel means no launcher to ask; restoring is then the only honest thing left.
            None => term::restore_now(),
        }
    }
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

        ctx.provide::<Tui>(tui.clone())
            .await
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;

        builtins::register(&ctx, &tui).await?;

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
