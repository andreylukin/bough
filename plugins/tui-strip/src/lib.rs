//! Invariant: the rail reads `about/line` by step-type NAME out of the ledger; it does NOT depend
//! on `bough-plugin-about-line` (P3-D11). A pane depending on a Consumer crate would invert the
//! seam rule, and the merge-extensible step-type map (§3) exists precisely so a renderer can read
//! a type it does not own. With `about-line` disabled the strip renders the glyph and no
//! about-lines.
//!
//! The intent half is ALWAYS rendered under its label, never as truth (§2).

pub mod invariant;
pub mod rail;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::events::{
    AgentCreated, AgentDisposed, AgentStatusChanged, AgentWake, Phase,
};
use bough_plugin_agents::{AgentId, Agents, Status};
use bough_plugin_ledger::{Ledger, LedgerHandle, LedgerStep, Order, StepQuery, StepType, TrajId};
use bough_plugin_tui_render::about::ABOUT_LINE;
use bough_plugin_tui_shell::pane::{
    Pane, PaneCx, PaneEvent, PaneOutcome, PaneSpec, RenderCx, Slot, SlotSize,
};
use bough_plugin_tui_shell::{PaneId, Tui, TuiHandle};
use parking_lot::Mutex;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

pub use rail::{
    focus_for_hit, glyph, hit_for_agent, on_click, rail, row_lines, RailRow, INTENT_LABEL,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-strip";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StripConfig {
    pub width: u16,
    pub show_about: bool,
    pub about_lines: u16,
}

/// Re-exported from the render library, which owns it because both panes read it (§1).
pub use bough_plugin_tui_render::{about_from_step, AboutView};

/// The rail itself.
///
/// It holds a `RailRow` per live agent and nothing else: `render` is synchronous and queries
/// nothing, and the four listeners below are what keep the list current.
pub struct StripPane {
    cfg: Arc<StripConfig>,
    rows: Arc<Mutex<Vec<RailRow>>>,
}

impl StripPane {
    /// A rail over a shared row list. Public so a test can drive `handle` and `rows()` without a
    /// composed tree.
    pub fn new(cfg: Arc<StripConfig>, rows: Arc<Mutex<Vec<RailRow>>>) -> StripPane {
        StripPane { cfg, rows }
    }

    /// The rows the rail would draw right now.
    pub fn rows(&self) -> Vec<RailRow> {
        self.rows.lock().clone()
    }
}

#[async_trait::async_trait]
impl Pane for StripPane {
    fn render(&self, cx: &mut RenderCx<'_>) {
        let rows = self.rows.lock().clone();
        let area = cx.area;
        let theme = *cx.theme();
        let focused = cx.view.focused_agent.clone();
        let (lines, spans) = rail::rail(
            &rows,
            focused.as_ref(),
            self.cfg.show_about,
            self.cfg.about_lines,
            area.width,
            &theme,
        );
        // §16 at the surface: whatever state halves this frame put on screen are recorded, and
        // the invariant checks that every one of them came from a CITED about-line.
        invariant::record_frame(&rows);
        for (agent, top, height) in spans {
            if top >= area.height {
                break;
            }
            let h = height.min(area.height - top);
            cx.hit(
                Rect {
                    x: area.x,
                    y: area.y + top,
                    width: area.width,
                    height: h,
                },
                rail::hit_for_agent(&agent),
            );
        }
        cx.frame.render_widget(Paragraph::new(lines), area);
    }

    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        match ev {
            PaneEvent::Click { hit, .. } => rail::on_click(hit.as_ref()),
            PaneEvent::Tick => {
                let _ = cx;
                PaneOutcome::Ignored
            }
            _ => PaneOutcome::Ignored,
        }
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("click", "focus that agent")]
    }
}

/// PURE: what a status change does to the row list.
pub fn set_status(rows: &mut [RailRow], agent: &AgentId, status: Status) {
    if let Some(r) = rows.iter_mut().find(|r| &r.agent == agent) {
        r.status = status;
    }
}

/// PURE: what an `about/line` step does to the row list. A step on a trajectory the rail does not
/// know is ignored rather than creating a rowless rail entry.
pub fn set_about(rows: &mut [RailRow], traj: &TrajId, view: AboutView) {
    if let Some(r) = rows.iter_mut().find(|r| r.traj.as_ref() == Some(traj)) {
        r.about = Some(view);
    }
}

/// The row.
pub struct StripPlugin;

#[async_trait::async_trait]
impl Plugin for StripPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = StripConfig;

    fn inject() -> Inject {
        Inject::required(["tui", "agents", "ledger"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        if cfg.width == 0 {
            return reject("width must be > 0; a zero-cell rail shows no agent".to_string());
        }
        if cfg.show_about && cfg.about_lines == 0 {
            return reject(
                "about_lines must be > 0 when show_about is true; set show_about: false instead"
                    .to_string(),
            );
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let tui = ctx
            .get::<Tui>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let tui = TuiHandle(tui.0.clone());
        let agents = ctx
            .get::<Agents>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let ledger = LedgerHandle(ledger.0.clone());

        let rows: Arc<Mutex<Vec<RailRow>>> = Arc::new(Mutex::new(Vec::new()));
        // Whatever is already live when this row activates: the rail is a Consumer and must not
        // depend on having been loaded before the agents it draws.
        {
            let mut held = rows.lock();
            for a in agents.list() {
                held.push(row_for(&a));
            }
        }
        // Backfill each known agent's newest about-line, so a rail that mounts after a wake shows
        // what the ledger already knows rather than waiting for the next one.
        let known: Vec<RailRow> = rows.lock().clone();
        for row in known {
            if let Some(traj) = row.traj.clone() {
                if let Ok(steps) = ledger
                    .0
                    .steps(&StepQuery {
                        trajs: vec![traj.clone()],
                        kinds: vec![StepType::new(ABOUT_LINE)],
                        order: Order::SeqDesc,
                        limit: Some(1),
                        ..Default::default()
                    })
                    .await
                {
                    if let Some(view) = steps.first().and_then(about_from_step) {
                        set_about(&mut rows.lock(), &traj, view);
                    }
                }
            }
        }

        let pane = Arc::new(StripPane::new(cfg.clone(), rows.clone()));
        tui.register_pane(
            &ctx,
            PaneSpec {
                id: PaneId::new("tui.strip"),
                slot: Slot::Strip,
                order: 0,
                size: SlotSize::Cells(cfg.width),
                title: "agents".into(),
                focusable: true,
                pane: pane.clone(),
            },
        )
        .await?;

        // The four listeners §2.4 names. Each keeps the row list current so `render` can stay
        // synchronous and query nothing.
        let (r, t) = (rows.clone(), tui.clone());
        ctx.on::<AgentCreated, _, _>(move |agent| {
            let (r, t) = (r.clone(), t.clone());
            async move {
                let mut held = r.lock();
                if !held.iter().any(|x| x.agent == *agent.id()) {
                    held.push(row_for(&agent));
                }
                drop(held);
                t.redraw();
            }
        })
        .await?;

        let (r, t) = (rows.clone(), tui.clone());
        ctx.on::<AgentDisposed, _, _>(move |id| {
            let (r, t) = (r.clone(), t.clone());
            async move {
                if let Some(row) = r.lock().iter_mut().find(|x| x.agent == id) {
                    // Kept, not removed: a disposed agent's rail row is how Andrey sees that it
                    // is gone. `glyph` renders it dim.
                    row.disposed = true;
                    row.wake_pending = false;
                }
                t.redraw();
            }
        })
        .await?;

        let (r, t) = (rows.clone(), tui.clone());
        ctx.on::<AgentStatusChanged, _, _>(move |change| {
            let (r, t) = (r.clone(), t.clone());
            async move {
                set_status(&mut r.lock(), &change.agent, change.to);
                t.redraw();
            }
        })
        .await?;

        let (r, t) = (rows.clone(), tui.clone());
        ctx.on::<AgentWake, _, _>(move |ev| {
            let (r, t) = (r.clone(), t.clone());
            async move {
                if let Some(row) = r.lock().iter_mut().find(|x| x.agent == ev.agent) {
                    row.wake_pending = ev.phase == Phase::Start;
                }
                t.redraw();
            }
        })
        .await?;

        let (r, t) = (rows.clone(), tui.clone());
        ctx.on::<LedgerStep, _, _>(move |step| {
            let (r, t) = (r.clone(), t.clone());
            async move {
                // BY NAME (P3-D11). `about_from_step` returns `None` for every other type, so the
                // filter and the parse are one call.
                if let Some(view) = about_from_step(&step) {
                    set_about(&mut r.lock(), &step.traj, view);
                    t.redraw();
                }
            }
        })
        .await?;

        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

/// A fresh rail row for a live agent.
pub fn row_for(agent: &bough_plugin_agents::Agent) -> RailRow {
    RailRow {
        agent: agent.id().clone(),
        traj: Some(agent.traj().clone()),
        name: agent.name().to_string(),
        status: agent.status(),
        wake_pending: agent.has_pending_wake(),
        disposed: agent.is_disposed(),
        about: None,
    }
}

bough_kernel::register_plugin!(StripPlugin);
