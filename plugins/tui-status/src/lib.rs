//! Invariant: this row OWNS the status line and nothing else. It reads `ctx.ledger` and the
//! shell's handle, assembles a [`StatusView`], and draws one row — it never steers an agent and
//! never writes a step. Disabling the row by patch removes the line and reflows the layout, which
//! is the phase's SWAP gate (phase ux1 §2.5, §17).
//!
//! Every step type it reads is read BY NAME (P3-D11): the row must render a model and a cost with
//! `llm` or `model-policy` swapped out, so it depends on neither crate.

pub mod invariant;
pub mod status;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::events::{AgentWake, Phase};
use bough_plugin_ledger::{Ledger, LedgerHandle, LedgerStep, Order, Step, StepQuery, StepType};
use bough_plugin_tools::Workspace;
use bough_plugin_tui_shell::pane::{
    Pane, PaneCx, PaneEvent, PaneOutcome, PaneSpec, RenderCx, Slot, SlotSize,
};
use bough_plugin_tui_shell::{PaneId, Tui, TuiHandle};
use chrono::{DateTime, Utc};
use ratatui::widgets::Paragraph;

pub use status::{
    elide_path, fields, status_line, Field, StatusView, DROP_ORDER, RENDER_ORDER, UNKNOWN,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-status";
/// The pane id this row registers in [`bough_plugin_tui_shell::Slot::Status`].
pub const PANE_ID: &str = "tui.status";

/// The step the model and the context budget are read out of, BY NAME.
pub const REQUEST_HEADER: &str = "request/header";
/// The step the cost is summed out of, BY NAME.
pub const USAGE_ROUND: &str = "usage/round";

/// The row's config. Every deployment-varying value is here; nothing is a `DEFAULT_` constant.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusConfig {
    /// Longest cwd rendered before the middle is elided.
    pub cwd_max: u16,
    /// Spinner frames, as one string. Deployment-varying (a terminal without a good font).
    pub spinner: String,
    /// How long ONE spinner frame is held. The pane is ticked at the shell's `tui.tick_ms`, which
    /// is a different (coarser) cadence: this field is what decides the spin, so the two knobs do
    /// not have to agree. A `spinner_ms` finer than `tick_ms` simply spins once per tick.
    pub spinner_ms: u64,
    /// Key hints, in order, as `"key=meaning"` pairs. The hint list is config, not a constant,
    /// because it is the one chrome a user might want shortened.
    pub hints: Vec<String>,
}

/// PURE: a `"key=meaning"` hint, split. `None` names a malformed pair, which `validate` rejects
/// rather than rendering half of.
pub fn parse_hint(s: &str) -> Option<(String, String)> {
    let (k, v) = s.split_once('=')?;
    if k.trim().is_empty() || v.trim().is_empty() {
        return None;
    }
    Some((k.trim().to_string(), v.trim().to_string()))
}

/// PURE: what a `request/header` body says about the model and the context budget.
///
/// TOTAL: a header missing either half leaves that half alone rather than guessing, because a
/// guessed context percentage is exactly the lie this row exists not to tell.
pub fn header_facts(step: &Step) -> Option<(Option<String>, Option<u8>)> {
    if step.kind.as_str() != REQUEST_HEADER {
        return None;
    }
    let model = step
        .body
        .get("call")
        .and_then(|c| c.get("model"))
        .and_then(|m| m.as_str())
        .map(str::to_string);
    let budget = step.body.get("budget").and_then(|b| b.as_u64());
    let used = step.body.get("projection_tokens").and_then(|t| t.as_u64());
    let left = match (budget, used) {
        (Some(b), Some(u)) if b > 0 => Some((100u64.saturating_sub(u * 100 / b)).min(100) as u8),
        _ => None,
    };
    Some((model, left))
}

/// PURE: what a `usage/round` body cost, or `None` for every other step type AND for a round whose
/// cost is unknown — an unknown cost adds nothing to the total rather than adding zero.
pub fn cost_of(step: &Step) -> Option<f64> {
    if step.kind.as_str() != USAGE_ROUND {
        return None;
    }
    step.body.get("cost_usd").and_then(|c| c.as_f64())
}

/// The status pane.
pub struct StatusPane {
    cfg: Arc<StatusConfig>,
    view: parking_lot::Mutex<StatusView>,
    /// When the running turn started, for the elapsed clock (M32).
    since: parking_lot::Mutex<Option<DateTime<Utc>>>,
    /// How many ticks have gone by, for the spinner.
    tick: parking_lot::Mutex<u64>,
    /// When the spinner last advanced a frame. `spinner_ms` is measured against this, so the
    /// spin rate is the row's own config and not the shell's tick cadence.
    spun_at: parking_lot::Mutex<Option<DateTime<Utc>>>,
}

impl StatusPane {
    /// A pane over an empty view. Public so a test can drive it without a composed tree.
    pub fn new(cfg: Arc<StatusConfig>) -> StatusPane {
        let hints = cfg.hints.iter().filter_map(|h| parse_hint(h)).collect();
        let view = StatusView {
            product: format!("bough {}", env!("CARGO_PKG_VERSION")),
            cwd_max: cfg.cwd_max,
            hints,
            spinner_frame: cfg.spinner.chars().next().unwrap_or(' '),
            ..Default::default()
        };
        StatusPane {
            cfg,
            view: parking_lot::Mutex::new(view),
            since: parking_lot::Mutex::new(None),
            tick: parking_lot::Mutex::new(0),
            spun_at: parking_lot::Mutex::new(None),
        }
    }

    /// The view the line would draw right now.
    pub fn view(&self) -> StatusView {
        self.view.lock().clone()
    }

    /// Where the process is working, as `ctx.workspace` published it (B5).
    pub fn set_cwd(&self, cwd: PathBuf, home: Option<PathBuf>) {
        let mut v = self.view.lock();
        v.cwd = Some(cwd);
        v.home = home;
    }

    /// Fold one step in. Called by the listener AND by the activation backfill, so a row that
    /// mounts mid-session shows what the ledger already knows.
    pub fn absorb(&self, step: &Step) {
        let mut v = self.view.lock();
        if let Some((model, left)) = header_facts(step) {
            if model.is_some() {
                v.model = model;
            }
            if left.is_some() {
                v.context_left = left;
            }
        }
        if let Some(c) = cost_of(step) {
            v.cost_usd = Some(v.cost_usd.unwrap_or(0.0) + c);
        }
    }

    /// A turn started or ended.
    pub fn set_running(&self, running: bool, now: DateTime<Utc>) {
        *self.since.lock() = if running { Some(now) } else { None };
        let mut v = self.view.lock();
        v.running = running;
        v.elapsed = if running { Some(Duration::ZERO) } else { None };
    }

    /// One spinner step and one elapsed step, from a clock the CALLER owns (a pane never reads
    /// one: `render` is synchronous and `handle` is where time arrives).
    pub fn tick(&self, now: DateTime<Utc>) {
        // ONE frame per `spinner_ms`, not one per shell tick. The two cadences are independent
        // knobs and a patch that sets `spinner_ms` has to move the spinner.
        let due = {
            let mut last = self.spun_at.lock();
            let ms = (now - last.unwrap_or(now - chrono::Duration::days(1)))
                .num_milliseconds()
                .max(0) as u64;
            let due = last.is_none() || ms >= self.cfg.spinner_ms;
            if due {
                *last = Some(now);
            }
            due
        };
        if due {
            let mut n = self.tick.lock();
            *n = n.wrapping_add(1);
            let frames: Vec<char> = self.cfg.spinner.chars().collect();
            let mut v = self.view.lock();
            if !frames.is_empty() {
                v.spinner_frame = frames[(*n as usize) % frames.len()];
            }
        }
        let mut v = self.view.lock();
        if let Some(started) = *self.since.lock() {
            let secs = (now - started).num_seconds().max(0) as u64;
            v.elapsed = Some(Duration::from_secs(secs));
        }
    }
}

#[async_trait::async_trait]
impl Pane for StatusPane {
    fn render(&self, cx: &mut RenderCx<'_>) {
        let area = cx.area;
        if area.width == 0 || area.height == 0 {
            return;
        }
        let view = self.view.lock().clone();
        let line = status::status_line(&view, area.width, cx.theme());
        invariant::record_frame(&view, &line, area.width);
        cx.frame.render_widget(Paragraph::new(line), area);
    }

    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        match ev {
            PaneEvent::Tick => {
                // Re-derive from the ONE authority (`TuiHandle::running`, focused-agent scoped)
                // so a focus change between agents cannot leave the spinner behind.
                let running = cx.tui.running();
                if running != self.view.lock().running {
                    self.set_running(running, cx.tui.running_since().unwrap_or(cx.at));
                }
                self.tick(cx.at);
                // Not `Handled`: a tick is not input, and claiming it would stop the shell from
                // handing the same tick to every other pane.
                PaneOutcome::Ignored
            }
            _ => PaneOutcome::Ignored,
        }
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }
}

/// The row.
pub struct TuiStatusPlugin;

#[async_trait::async_trait]
impl Plugin for TuiStatusPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = StatusConfig;

    fn inject() -> Inject {
        Inject::required(["tui", "ledger"]).union(&Inject::optional(["agents", "workspace"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        if cfg.cwd_max == 0 {
            return reject(
                "cwd_max must be > 0; a zero-cell cwd is the one field B5 exists to show"
                    .to_string(),
            );
        }
        if cfg.spinner.chars().count() == 0 {
            return reject(
                "spinner must have at least one frame; set it to a single character for a \
                 terminal without a good font"
                    .to_string(),
            );
        }
        if cfg.spinner_ms == 0 {
            return reject("spinner_ms must be > 0; a zero interval spins every frame".to_string());
        }
        if let Some(bad) = cfg.hints.iter().find(|h| parse_hint(h).is_none()) {
            return reject(format!(
                "hint {bad:?} is not a `key=meaning` pair; a half-parsed hint would render as \
                 chrome nobody can act on"
            ));
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let tui = ctx
            .get::<Tui>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let tui = TuiHandle(tui.0.clone());
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let ledger = LedgerHandle(ledger.0.clone());

        let pane = Arc::new(StatusPane::new(cfg.clone()));

        // B5 at the surface: the cwd on the line is the SAME object the tools resolve against,
        // because it comes from `ctx.workspace` and not from `std::env::current_dir`.
        if let Ok(root) = ctx.get::<Workspace>() {
            pane.set_cwd(root.path().to_path_buf(), Some(bough_util::home_dir()));
        }

        // Backfill: the newest header, and every cost this ledger holds. A status line that shows
        // `—` for a session it could have read is the same bug as one that shows a zero.
        for kind in [REQUEST_HEADER, USAGE_ROUND] {
            let limit = if kind == REQUEST_HEADER {
                Some(1)
            } else {
                None
            };
            if let Ok(steps) = ledger
                .0
                .steps(&StepQuery {
                    kinds: vec![StepType::new(kind)],
                    order: Order::SeqDesc,
                    limit,
                    ..Default::default()
                })
                .await
            {
                for step in steps.iter() {
                    pane.absorb(step);
                }
            }
        }

        tui.register_pane(
            &ctx,
            PaneSpec {
                id: PaneId::new(PANE_ID),
                slot: Slot::Status,
                order: 0,
                size: SlotSize::Cells(1),
                title: "status".into(),
                // The status line never takes the keyboard: it is chrome, and B1's whole lesson is
                // that focus goes where the user put it.
                focusable: false,
                pane: pane.clone(),
            },
        )
        .await?;

        let (p, t) = (pane.clone(), tui.clone());
        ctx.on::<LedgerStep, _, _>(move |step| {
            let (p, t) = (p.clone(), t.clone());
            async move {
                let before = p.view();
                p.absorb(&step);
                if p.view() != before {
                    t.redraw();
                }
            }
        })
        .await?;

        // The recorded frame is per-process and this row owns it: unloading forgets what it drew,
        // so a reload is never checked against its predecessor's screen (§0.2).
        ctx.effect(|e| async move {
            e.defer_sync(invariant::forget);
            Ok(())
        })
        .await?;

        let (p, t) = (pane.clone(), tui.clone());
        ctx.on::<AgentWake, _, _>(move |ev| {
            let (p, t) = (p.clone(), t.clone());
            async move {
                // §2.5: the spinner and the `esc to interrupt` hint are about the FOCUSED agent.
                // Unfiltered, terra's background wake started sol's clock and terra's `wake/end`
                // stopped it while sol was still answering — and the chrome then disagreed with
                // `TuiHandle::running()`, which is what decides whether Esc means interrupt.
                if t.focused_agent().as_ref() != Some(&ev.agent) {
                    return;
                }
                p.set_running(ev.phase == Phase::Start, Utc::now());
                t.redraw();
            }
        })
        .await?;

        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(TuiStatusPlugin);
