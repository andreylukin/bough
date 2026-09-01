//! The cost pane (§11, an Aux Consumer): where the money went, by lane, over time.
//!
//! The rail's `$0.16` was one ambiguous number; the 2026-09-01 brainstorm asked for the two
//! things it hides — WHO is spending (per-lane attribution, background work included) and the
//! SHAPE of the spend (bursts vs drip). Everything here folds from `usage/round` steps, which
//! already carry `cost_usd`, tokens and the model per trajectory: no new collection, one query.
//!
//! Summoned with `/cost`, zero rows otherwise (the search pane's collapse pattern). `w` cycles
//! the window (session · today · week); the chart is STACKED bars — bursty spend overlaid as
//! lines is spaghetti at terminal resolution, stacked it reads as "total, and who".

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_commands::{
    Command, CommandCx, CommandError, CommandName, CommandOutput, CommandScope, CommandSpec,
    Commands, Invocation, OutputRender,
};
use bough_plugin_ledger::{Ledger, LedgerHandle, LedgerStep, Order, StepQuery, StepType};
use bough_plugin_tui_shell::pane::{
    Pane, PaneCx, PaneEvent, PaneOutcome, PaneSpec, RenderCx, Slot, SlotSize,
};
use bough_plugin_tui_shell::{PaneId, Theme, Tui, TuiHandle};
use chrono::{DateTime, Duration, Utc};
use crossterm::event::KeyCode;
use parking_lot::Mutex;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::sync::atomic::{AtomicBool, Ordering};

pub const PLUGIN_NAME: &str = "tui-cost";
pub const PANE_ID: &str = "tui.cost";
/// The step type everything folds from, BY NAME (P3-D11's rule).
pub const USAGE_STEP: &str = "usage/round";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CostConfig {
    /// Rows the pane takes when open.
    pub height: u16,
    /// How many `usage/round` steps the activation backfill reads (newest first). Live steps
    /// accumulate on top, so this bounds HISTORY depth, not accuracy of the session.
    #[serde(default = "default_backfill")]
    pub backfill: usize,
}
fn default_backfill() -> usize {
    4000
}

/// One priced round, reduced to what the pane folds.
#[derive(Clone, Debug, PartialEq)]
pub struct Round {
    pub at: DateTime<Utc>,
    pub series: String,
    pub cost: f64,
    pub input: u64,
    pub cached: u64,
}

/// PURE: which SERIES a trajectory's spend belongs to. Lanes by name (a merged head
/// `lane/trunk+lane/sol` is still trunk's story), every worker together, and anything else —
/// summarizers, background passes — is `sys`, a first-class series rather than smeared spend.
pub fn series_of(traj: &str) -> String {
    if traj.contains("worker") {
        return "workers".to_string();
    }
    if let Some(rest) = traj.strip_prefix("lane/") {
        let name = rest.split('+').next().unwrap_or(rest);
        return name.strip_prefix("lane/").unwrap_or(name).to_string();
    }
    "sys".to_string()
}

/// PURE: a `usage/round` step as a [`Round`], or `None` for every other step type.
pub fn round_of(step: &bough_plugin_ledger::Step) -> Option<Round> {
    if step.kind.as_str() != USAGE_STEP {
        return None;
    }
    let get = |k: &str| step.body.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    Some(Round {
        at: step.at,
        series: series_of(step.traj.as_str()),
        cost: step.body.get("cost_usd").and_then(|v| v.as_f64())?,
        input: get("input_tokens"),
        cached: get("cache_read_tokens"),
    })
}

/// The window the fold covers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Window {
    Session,
    Today,
    Week,
}

impl Window {
    fn next(self) -> Window {
        match self {
            Window::Session => Window::Today,
            Window::Today => Window::Week,
            Window::Week => Window::Session,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Window::Session => "session",
            Window::Today => "today",
            Window::Week => "week",
        }
    }
    fn since(self, now: DateTime<Utc>, session_start: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Window::Session => session_start,
            Window::Today => now - Duration::hours(24),
            Window::Week => now - Duration::days(7),
        }
    }
}

/// One series' fold over the window, table-ready.
#[derive(Clone, Debug, PartialEq)]
pub struct SeriesTotal {
    pub name: String,
    pub cost: f64,
    /// Cached share of input: `cached / (input + cached)` — the cache-health number the
    /// caching battles taught us to watch. `None` with no input at all.
    pub cached_share: Option<f64>,
}

/// PURE: the whole fold — per-series totals (spend-desc) and the stacked chart columns.
/// Each column is one time bucket holding `(series index, cost)` pairs, oldest first.
pub fn fold(
    rounds: &[Round],
    since: DateTime<Utc>,
    now: DateTime<Utc>,
    columns: usize,
) -> (Vec<SeriesTotal>, Vec<Vec<(usize, f64)>>) {
    let mut totals: Vec<SeriesTotal> = Vec::new();
    let mut inputs: Vec<(u64, u64)> = Vec::new();
    for r in rounds.iter().filter(|r| r.at >= since && r.at <= now) {
        let i = match totals.iter().position(|t| t.name == r.series) {
            Some(i) => i,
            None => {
                totals.push(SeriesTotal {
                    name: r.series.clone(),
                    cost: 0.0,
                    cached_share: None,
                });
                inputs.push((0, 0));
                totals.len() - 1
            }
        };
        totals[i].cost += r.cost;
        inputs[i].0 += r.input;
        inputs[i].1 += r.cached;
    }
    for (t, (input, cached)) in totals.iter_mut().zip(&inputs) {
        let denom = input + cached;
        t.cached_share = (denom > 0).then(|| *cached as f64 / denom as f64);
    }
    // Spend-desc, and the CHART indexes into this order, so color and rank agree everywhere.
    let mut order: Vec<usize> = (0..totals.len()).collect();
    order.sort_by(|a, b| totals[*b].cost.total_cmp(&totals[*a].cost));
    let totals: Vec<SeriesTotal> = order.iter().map(|i| totals[*i].clone()).collect();

    let span = (now - since).num_milliseconds().max(1);
    let mut chart: Vec<Vec<(usize, f64)>> = vec![Vec::new(); columns.max(1)];
    for r in rounds.iter().filter(|r| r.at >= since && r.at <= now) {
        let Some(series) = totals.iter().position(|t| t.name == r.series) else {
            continue;
        };
        let frac = (r.at - since).num_milliseconds() as f64 / span as f64;
        let col = ((frac * columns as f64) as usize).min(columns.saturating_sub(1));
        match chart[col].iter_mut().find(|(s, _)| *s == series) {
            Some((_, c)) => *c += r.cost,
            None => chart[col].push((series, r.cost)),
        }
    }
    // Stack order = series order, so the biggest spender is always the bottom band.
    for col in &mut chart {
        col.sort_by_key(|(s, _)| *s);
    }
    (totals, chart)
}

/// A dollar amount that never rounds a real cost to `$0.00` (the status pane's rule) — and
/// never shows `-0.00` for a float hair below zero.
pub fn money(c: f64) -> String {
    if c.abs() < 0.00005 {
        "$0.00".to_string()
    } else if c > 0.0 && c < 0.01 {
        format!("${c:.4}")
    } else {
        format!("${c:.2}")
    }
}

/// The pane.
pub struct CostPane {
    cfg: Arc<CostConfig>,
    rounds: Mutex<Vec<Round>>,
    open: AtomicBool,
    window: Mutex<Window>,
    session_start: DateTime<Utc>,
    /// Trajectory segment → the agent's CURRENT name, from the `agents` rows: `lane/sol` is
    /// trunk's story twice over (the rename kept trajectory spellings; a merge concatenates
    /// them), and the table should say who spends in today's names.
    alias: Mutex<std::collections::BTreeMap<String, String>>,
}

impl CostPane {
    pub fn new(cfg: Arc<CostConfig>) -> CostPane {
        CostPane {
            cfg,
            rounds: Mutex::new(Vec::new()),
            open: AtomicBool::new(false),
            window: Mutex::new(Window::Session),
            session_start: Utc::now(),
            alias: Mutex::new(std::collections::BTreeMap::new()),
        }
    }

    /// Teach the pane whose story each trajectory segment is.
    pub fn learn_names(&self, rows: &[bough_plugin_ledger::AgentRow]) {
        let mut alias = self.alias.lock();
        for row in rows {
            for segment in row.traj.as_str().split('+') {
                alias.insert(segment.to_string(), row.name.to_string());
            }
        }
    }

    pub fn absorb(&self, step: &bough_plugin_ledger::Step) -> bool {
        match round_of(step) {
            Some(mut r) => {
                if r.series != "workers" && r.series != "sys" {
                    if let Some(segment) = step.traj.as_str().split('+').next() {
                        if let Some(name) = self.alias.lock().get(segment) {
                            r.series = name.clone();
                        }
                    }
                }
                self.rounds.lock().push(r);
                true
            }
            None => false,
        }
    }

    pub fn toggle(&self) -> bool {
        let now_open = !self.open.load(Ordering::SeqCst);
        self.open.store(now_open, Ordering::SeqCst);
        now_open
    }

    fn series_color(theme: &Theme, i: usize) -> Color {
        [
            theme.accent,
            theme.added,
            theme.warn,
            theme.interactive,
            theme.thought,
            theme.removed,
        ][i % 6]
    }
}

/// The eighth-block a fractional TOP cell renders as; a full band cell is `█`.
fn eighth(frac: f64) -> char {
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    BLOCKS[((frac * 8.0) as usize).clamp(0, 7)]
}

#[async_trait::async_trait]
impl Pane for CostPane {
    fn render(&self, cx: &mut RenderCx<'_>) {
        if !self.open.load(Ordering::SeqCst) {
            cx.report_aux_rows(0);
            return;
        }
        cx.report_aux_rows(self.cfg.height);
        let area = cx.area;
        if area.width < 20 || area.height == 0 {
            return;
        }
        let theme = *cx.theme();
        let now = cx.view.now;
        let window = *self.window.lock();
        let since = window.since(now, self.session_start);
        let rounds = self.rounds.lock().clone();
        let chart_w = area.width.saturating_sub(2) as usize;
        let (totals, chart) = fold(&rounds, since, now, chart_w);
        let total: f64 = totals.iter().map(|t| t.cost).sum();

        let mut lines: Vec<Line<'static>> = Vec::new();
        // Header: window · total · the key that changes the window.
        lines.push(Line::from(vec![
            Span::styled(
                format!(" cost · {} ", window.label()),
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(money(total), Style::default().fg(theme.accent)),
            Span::styled(
                "   w: window · /cost: close".to_string(),
                Style::default().fg(theme.dim),
            ),
        ]));

        // The stacked chart. Height rows; each column's bands bottom-up in series colors, the
        // topmost partial as an eighth-block so a small burst is still a visible tick.
        let table_rows = totals.len().clamp(1, 4) as u16;
        let chart_h = area.height.saturating_sub(1 + table_rows).max(1);
        let peak = chart
            .iter()
            .map(|col| col.iter().map(|(_, c)| c).sum::<f64>())
            .fold(0.0_f64, f64::max);
        let mut grid: Vec<Vec<(char, Color)>> =
            vec![vec![(' ', theme.dim); chart_w]; chart_h as usize];
        if peak > 0.0 {
            for (x, col) in chart.iter().enumerate() {
                let mut cells = 0.0_f64;
                for (series, cost) in col {
                    let band = cost / peak * chart_h as f64;
                    let color = Self::series_color(&theme, *series);
                    let from = cells;
                    cells += band;
                    let mut y = from;
                    while y < cells.min(chart_h as f64) {
                        let row = y.floor() as usize;
                        let fill = (cells - y).min(1.0);
                        let ch = if fill >= 0.999 { '█' } else { eighth(fill) };
                        grid[row][x] = (ch, color);
                        y = (row + 1) as f64;
                    }
                }
            }
        }
        for row in grid.iter().rev() {
            let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
            for (ch, color) in row {
                spans.push(Span::styled(ch.to_string(), Style::default().fg(*color)));
            }
            lines.push(Line::from(spans));
        }

        // The table: rank = color = stack order. Cached share is the cache-health number.
        for (i, t) in totals.iter().take(table_rows as usize).enumerate() {
            let cached = match t.cached_share {
                Some(s) => format!("{:>3.0}% cached", s * 100.0),
                None => "  — cached".to_string(),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    " █ ".to_string(),
                    Style::default().fg(Self::series_color(&theme, i)),
                ),
                Span::styled(format!("{:<12}", t.name), Style::default().fg(theme.fg)),
                Span::styled(
                    format!("{:>9}", money(t.cost)),
                    Style::default().fg(theme.fg),
                ),
                Span::styled(format!("  {cached}"), Style::default().fg(theme.dim)),
            ]));
        }
        if totals.is_empty() {
            lines.push(Line::from(Span::styled(
                " nothing spent in this window".to_string(),
                Style::default().fg(theme.dim),
            )));
        }
        cx.frame.render_widget(Paragraph::new(lines), area);
    }

    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        match ev {
            PaneEvent::Key(key) if key.code == KeyCode::Char('w') => {
                let mut w = self.window.lock();
                *w = w.next();
                drop(w);
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            PaneEvent::Key(key) if key.code == KeyCode::Esc => {
                self.open.store(false, Ordering::SeqCst);
                cx.tui.focus_composer();
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            _ => PaneOutcome::Ignored,
        }
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("w", "window"), ("esc", "close")]
    }
}

struct CostPaneArc(Arc<CostPane>);

#[async_trait::async_trait]
impl Pane for CostPaneArc {
    fn render(&self, cx: &mut RenderCx<'_>) {
        self.0.render(cx)
    }
    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        self.0.handle(ev, cx).await
    }
    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        self.0.key_hints()
    }
}

/// `/cost` — the toggle.
struct CostCommand {
    pane: Arc<CostPane>,
    tui: TuiHandle,
}

#[async_trait::async_trait]
impl Command for CostCommand {
    async fn run(&self, _inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let open = self.pane.toggle();
        if open {
            // The keyboard moves INTO the pane: the focused Aux pane gets its registered rows
            // (`layout_with`'s focused rule), which is what breaks the closed pane's 0-row
            // report; it also puts `w` and Esc where the hints say they are.
            self.tui.focus_pane(PaneId::new(PANE_ID)).await;
        }
        self.tui.redraw();
        Ok(CommandOutput {
            text: if open {
                "cost pane open — w cycles session/today/week".to_string()
            } else {
                "cost pane closed".to_string()
            },
            render: OutputRender::Plain,
            cites: Vec::new(),
        })
    }
}

/// The row.
pub struct CostPlugin;

#[async_trait::async_trait]
impl Plugin for CostPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = CostConfig;

    fn inject() -> Inject {
        Inject::required(["tui", "ledger"]).union(&Inject::optional(["commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        if cfg.height == 0 {
            return Err(ConfigError::Rejected {
                detail: "height must be > 0; a zero-row pane can show nothing".to_string(),
            });
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

        let pane = Arc::new(CostPane::new(cfg.clone()));
        if let Ok(rows) = ledger.0.agents().await {
            pane.learn_names(&rows);
        }

        // Backfill: one query, newest first, every trajectory (`trajs: []` is "no filter").
        if let Ok(mut steps) = ledger
            .0
            .steps(&StepQuery {
                kinds: vec![StepType::new(USAGE_STEP)],
                order: Order::SeqDesc,
                limit: Some(cfg.backfill),
                ..Default::default()
            })
            .await
        {
            steps.reverse();
            for s in &steps {
                pane.absorb(s);
            }
        }

        let (p2, t2) = (Arc::clone(&pane), tui.clone());
        ctx.on::<LedgerStep, _, _>(move |step| {
            let (pane, tui) = (Arc::clone(&p2), t2.clone());
            async move {
                if pane.absorb(&step) && pane.open.load(Ordering::SeqCst) {
                    tui.redraw();
                }
            }
        })
        .await?;

        tui.register_pane(
            &ctx,
            PaneSpec {
                id: PaneId::new(PANE_ID),
                slot: Slot::Aux,
                order: 1,
                size: SlotSize::Cells(cfg.height),
                title: "cost".into(),
                focusable: true,
                pane: Arc::new(CostPaneArc(Arc::clone(&pane))),
            },
        )
        .await?;

        // OPTIONAL like the rail's graph: no commands row, no `/cost`, pane still composes.
        if let Ok(commands) = ctx.get::<Commands>() {
            commands
                .register(
                    &ctx,
                    CommandSpec {
                        name: CommandName::new("cost"),
                        summary: "where the money went, and who spent it".to_string(),
                        usage: "/cost".to_string(),
                        args: schemars::SchemaGenerator::default().into_root_schema_for::<NoArgs>(),
                        scope: CommandScope::Global,
                        run: Arc::new(CostCommand { pane, tui }),
                    },
                )
                .await
                .map_err(|e| PluginError::new(entry.clone(), e))?;
        }
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}

/// `/cost` takes nothing.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct NoArgs {}

bough_kernel::register_plugin!(CostPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    fn round(at_min: i64, series: &str, cost: f64, input: u64, cached: u64) -> Round {
        Round {
            at: DateTime::<Utc>::from_timestamp(at_min * 60, 0).unwrap(),
            series: series.to_string(),
            cost,
            input,
            cached,
        }
    }

    #[test]
    fn series_names_lanes_workers_and_sys() {
        assert_eq!(series_of("lane/trunk"), "trunk");
        assert_eq!(series_of("lane/trunk+lane/sol"), "trunk");
        assert_eq!(series_of("worker-01a058fd"), "workers");
        assert_eq!(series_of("lane/roots/worker-01a0"), "workers");
        assert_eq!(series_of("tuner/anything"), "sys");
    }

    #[test]
    fn the_fold_ranks_by_spend_and_reports_cache_share() {
        let now = DateTime::<Utc>::from_timestamp(100 * 60, 0).unwrap();
        let since = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
        let rounds = vec![
            round(10, "trunk", 0.02, 1000, 3000),
            round(50, "roots", 0.30, 2000, 0),
            round(90, "trunk", 0.03, 1000, 1000),
        ];
        let (totals, chart) = fold(&rounds, since, now, 10);
        assert_eq!(totals[0].name, "roots", "the biggest spender ranks first");
        assert!((totals[0].cost - 0.30).abs() < 1e-9);
        assert!((totals[1].cost - 0.05).abs() < 1e-9);
        // trunk: cached 4000 of 6000 total input-side tokens.
        assert!((totals[1].cached_share.unwrap() - 4000.0 / 6000.0).abs() < 1e-9);
        // Buckets land oldest-first: minute 10 of 100 → column 1, minute 90 → column 9.
        assert!(chart[1].iter().any(|(s, _)| totals[*s].name == "trunk"));
        assert!(chart[9].iter().any(|(s, _)| totals[*s].name == "trunk"));
        assert!(chart[5].iter().any(|(s, _)| totals[*s].name == "roots"));
    }

    #[test]
    fn a_round_outside_the_window_is_not_counted() {
        let now = DateTime::<Utc>::from_timestamp(100 * 60, 0).unwrap();
        let since = DateTime::<Utc>::from_timestamp(60 * 60, 0).unwrap();
        let rounds = vec![
            round(10, "trunk", 5.0, 0, 0),
            round(80, "trunk", 0.10, 0, 0),
        ];
        let (totals, _) = fold(&rounds, since, now, 10);
        assert!((totals[0].cost - 0.10).abs() < 1e-9);
    }

    #[test]
    fn money_never_shows_a_real_cost_as_zero() {
        assert_eq!(money(0.0044), "$0.0044");
        assert_eq!(money(1.24), "$1.24");
        assert_eq!(money(0.0), "$0.00");
    }
}
