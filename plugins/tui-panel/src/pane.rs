//! Invariant: `render` is a PURE function of the state the pane already holds; every read (the
//! kernel, the seams, the ui file) happens in `refresh`, an EFFECT of the row, one at a time —
//! and every write the panel performs goes through `store::write` into the ui layer, NEVER into
//! the live tree ($BOUGH_HOME's watch is the one apply path, for the panel and a human edit
//! alike). A failed read or write is rendered on the pane, never a silently blank surface.

use std::sync::Arc;

use bough_plugin_ledger::{AgentName, LedgerHandle, Order, StepQuery, StepType};
use bough_plugin_llm::{LlmHandle, ModelMatch};
use bough_plugin_mcp::{McpHandle, ServerName};
use bough_plugin_schedule::{JobName, JobOutcome, ScheduleHandle};
use bough_plugin_tui_shell::pane::{Pane, PaneCx, PaneEvent, PaneOutcome, RenderCx};
use bough_plugin_tui_shell::TuiHandle;
use parking_lot::Mutex;
use ratatui::widgets::Paragraph;

use crate::data::{self, AdapterRow, JobFacts, ModelData, PanelData, SeamFacts};
use crate::state::{on_key, Action, PanelState, Tab};
use crate::store;
use crate::PanelConfig;

/// The step kinds the model tab reads BY NAME, so the panel and the status line cannot disagree
/// about what actually ran (the `tui-status` precedent).
const REQUEST_HEADER: &str = "request/header";

pub struct PanelPane {
    pub cfg: Arc<PanelConfig>,
    pub state: Mutex<PanelState>,
    ctx: Option<bough_kernel::Context>,
    mcp: Option<McpHandle>,
    schedule: Option<ScheduleHandle>,
    ledger: Option<LedgerHandle>,
    llm: Option<LlmHandle>,
}

impl PanelPane {
    pub fn new(cfg: Arc<PanelConfig>) -> PanelPane {
        PanelPane {
            cfg,
            state: Mutex::new(PanelState::default()),
            ctx: None,
            mcp: None,
            schedule: None,
            ledger: None,
            llm: None,
        }
    }

    pub fn with_ctx(mut self, ctx: bough_kernel::Context) -> PanelPane {
        self.ctx = Some(ctx);
        self
    }

    pub fn with_seams(
        mut self,
        mcp: Option<McpHandle>,
        schedule: Option<ScheduleHandle>,
        ledger: Option<LedgerHandle>,
        llm: Option<LlmHandle>,
    ) -> PanelPane {
        self.mcp = mcp;
        self.schedule = schedule;
        self.ledger = ledger;
        self.llm = llm;
        self
    }

    /// Open on `tab` and take a fresh read. The commands' and `^t`'s one entry point.
    pub fn open(self: &Arc<Self>, tui: TuiHandle, tab: Tab) {
        {
            let mut st = self.state.lock();
            st.open = true;
            st.switch(tab);
        }
        self.refresh(tui);
    }

    /// Take a fresh read of everything the tabs show. ONE at a time; an EFFECT of the row, so a
    /// refresh armed just before this row is disabled cannot call a seam after disposal.
    pub fn refresh(self: &Arc<Self>, tui: TuiHandle) {
        let Some(ctx) = self.ctx.clone() else {
            return; // a bare unit test: state-only, no seams to read
        };
        {
            let mut st = self.state.lock();
            if st.refreshing {
                return;
            }
            st.refreshing = true;
        }
        let me = Arc::clone(self);
        ctx.clone().effect_spawn(move |ectx| async move {
            if ectx.checkpoint().await.is_err() {
                me.state.lock().refreshing = false;
                return Ok(());
            }
            let taken = me.gather(&ctx).await;
            {
                let mut st = me.state.lock();
                st.refreshing = false;
                st.refreshed_at = Some(chrono::Utc::now());
                match taken {
                    Ok((data, store_err)) => {
                        st.data = Some(data);
                        st.store_error = store_err;
                        st.error = None;
                        st.clamp_cursor();
                    }
                    Err(e) => st.error = Some(e),
                }
            }
            tui.redraw();
            Ok(())
        });
    }

    /// The one read. Joins the kernel's two truths (composition and snapshot) with the seams'
    /// live facts and the ui diff into plain data the pure builders take.
    async fn gather(
        &self,
        ctx: &bough_kernel::Context,
    ) -> Result<(PanelData, Option<String>), String> {
        let kernel = ctx.kernel().ok_or("no kernel behind this context")?;
        let comp = kernel.composition().ok_or("no composition is loaded yet")?;
        let snap = data::snap_index(&kernel.rows_snapshot());

        let ui_path = bough_util::ui_patch_path();
        let (ui, store_err) = match store::read(&ui_path) {
            Ok(u) => (u, None),
            Err(e) => (store::UiEntries::new(), Some(e.to_string())),
        };

        let rows = data::config_rows(&comp, &snap, &ui);
        let known_ids = data::known_ids(&comp);
        let raw_dump = bough_kernel::render(&comp, bough_kernel::DumpFormat::Yaml);
        let warnings = comp
            .warnings
            .iter()
            .map(|w| match w {
                bough_kernel::ComposeWarning::AbsentRowId { layer, id } => {
                    format!("layer `{layer}` patches row `{id}`, which no layer created")
                }
            })
            .collect();

        let mut seam = SeamFacts::default();
        if let Some(mcp) = &self.mcp {
            for name in mcp.servers() {
                let ready = mcp.is_ready(&name);
                // The cache fills on first use and is refreshed by `r`; a miss on a live server
                // is one list call, not one per frame (render never reaches a seam).
                let tools = mcp.tools(Some(&name)).await.ok().map(|t| t.len());
                seam.servers.insert(name.to_string(), (ready, tools));
            }
        }
        let jobs: Vec<(String, JobFacts)> = match &self.schedule {
            Some(s) => s
                .0
                .jobs()
                .into_iter()
                .map(|j| {
                    (
                        j.owner.to_string(),
                        JobFacts {
                            name: j.name.to_string(),
                            next: j.next,
                            last: j.last.map(|run| {
                                let what = match run.outcome {
                                    JobOutcome::Ran { detail } => detail,
                                    JobOutcome::Pending { reason } => format!("pending: {reason}"),
                                    JobOutcome::Failed { error } => format!("failed: {error}"),
                                };
                                format!("last {} · {}", run.at.format("%H:%M:%S"), what)
                            }),
                        },
                    )
                })
                .collect(),
            None => Vec::new(),
        };
        let (servers, collectors) = data::connector_rows(&comp, &snap, &seam, &jobs);

        let mut model = ModelData::default();
        if let Some(cfg) = data::policy_of(&comp) {
            model.sol = Some(cfg.sol.clone());
            model.terra = Some(cfg.terra.clone());
            if let Some(ledger) = &self.ledger {
                let agents = ledger.0.agents().await.map_err(|e| e.to_string())?;
                let pairs: Vec<(String, Option<String>)> = agents
                    .into_iter()
                    .map(|a| (a.name.to_string(), a.model_override))
                    .collect();
                model.agents = data::agent_rows(&cfg, &pairs);
            }
        }
        if let Some(llm) = &self.llm {
            model.adapters = llm
                .adapters()
                .into_iter()
                .map(|(name, m)| AdapterRow {
                    name: name.to_string(),
                    claim: match m {
                        ModelMatch::Any => "*".to_string(),
                        ModelMatch::Prefix(p) => format!("{p}*"),
                        ModelMatch::Exact(e) => e,
                    },
                })
                .collect();
        }
        model.env_keys = data::env_key_names(&comp)
            .into_iter()
            .map(|name| {
                let set = std::env::var(&name).is_ok_and(|v| !v.is_empty());
                (name, set)
            })
            .collect();
        if let Some(ledger) = &self.ledger {
            let steps = ledger
                .0
                .steps(&StepQuery {
                    kinds: vec![StepType::new(REQUEST_HEADER)],
                    order: Order::SeqDesc,
                    limit: Some(1),
                    ..Default::default()
                })
                .await
                .map_err(|e| e.to_string())?;
            model.last_model = steps.first().and_then(|s| {
                s.body
                    .get("call")
                    .and_then(|c| c.get("model"))
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
            });
        }

        Ok((
            PanelData {
                fingerprint: comp.fingerprint.to_string(),
                layers: comp.layers.iter().map(|l| l.to_string()).collect(),
                warnings,
                rows,
                raw_dump,
                servers,
                collectors,
                model,
                known_ids,
                ui,
                taken_at: Some(chrono::Utc::now()),
            },
            store_err,
        ))
    }

    /// Flip one row in the ui layer. Reads the file fresh (a human may have edited it since the
    /// last refresh), applies the toggle rule, prunes, writes-then-renames, and records the
    /// document for the invariant. The RESULT arrives as a `config/reload` like any other edit.
    pub fn toggle(&self, id: &str, effective_disabled: bool) {
        let path = bough_util::ui_patch_path();
        let outcome = store::read(&path).and_then(|entries| {
            let known = self
                .state
                .lock()
                .data
                .as_ref()
                .map(|d| d.known_ids.clone())
                .unwrap_or_default();
            let next = store::toggled(&entries, id, effective_disabled, &known);
            store::write(&path, &next)?;
            crate::invariant::record(store::render(&next), known);
            Ok(next)
        });
        let mut st = self.state.lock();
        match outcome {
            Ok(next) => {
                st.store_error = None;
                st.banner = Some(match next.get(id) {
                    Some(true) => format!("pinned {id} off · waiting for the reload"),
                    Some(false) => format!("pinned {id} on · waiting for the reload"),
                    None => format!("withdrew the pin on {id} · waiting for the reload"),
                });
            }
            Err(e) => st.store_error = Some(e.to_string()),
        }
    }
}

/// The registered pane: an `Arc<PanelPane>` so `handle` can arm work that owns the pane (the
/// `tui-search` precedent, D-WP5-1).
pub struct PanelPaneArc(pub Arc<PanelPane>);

impl PanelPaneArc {
    /// Perform what a pure key decision asked for.
    async fn perform(&self, action: Action, cx: &PaneCx) -> PaneOutcome {
        match action {
            Action::Redraw => {
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            Action::Refresh => {
                self.0.refresh(cx.tui.clone());
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            Action::Close => {
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            Action::Toggle {
                id,
                effective_disabled,
            } => {
                self.0.toggle(&id, effective_disabled);
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            Action::ClearOverride { agent } => {
                let Some(ledger) = self.0.ledger.clone() else {
                    return PaneOutcome::Handled;
                };
                let name = AgentName::new(agent.as_str());
                let outcome = async {
                    let row = ledger
                        .0
                        .agent(&name)
                        .await
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| format!("no agents row named {name}"))?;
                    ledger
                        .0
                        .put_agent(bough_plugin_ledger::AgentRow {
                            model_override: None,
                            ..row
                        })
                        .await
                        .map_err(|e| e.to_string())
                }
                .await;
                {
                    let mut st = self.0.state.lock();
                    match outcome {
                        Ok(()) => {
                            st.banner = Some(format!(
                                "cleared {agent}'s override · applies on its next wake"
                            ))
                        }
                        Err(e) => st.error = Some(e),
                    }
                }
                self.0.refresh(cx.tui.clone());
                PaneOutcome::Handled
            }
            Action::Sweep { job } => {
                let Some(schedule) = self.0.schedule.clone() else {
                    return PaneOutcome::Handled;
                };
                let outcome = schedule.0.fire_now(&JobName::new(job.as_str())).await;
                {
                    let mut st = self.0.state.lock();
                    match outcome {
                        Ok(run) => {
                            let what = match run.outcome {
                                JobOutcome::Ran { detail } => detail,
                                JobOutcome::Pending { reason } => format!("pending: {reason}"),
                                JobOutcome::Failed { error } => format!("failed: {error}"),
                            };
                            st.banner = Some(format!("swept {job} · {what}"));
                        }
                        Err(e) => st.error = Some(e.to_string()),
                    }
                }
                self.0.refresh(cx.tui.clone());
                PaneOutcome::Handled
            }
            Action::RefreshTools { server } => {
                let Some(mcp) = self.0.mcp.clone() else {
                    return PaneOutcome::Handled;
                };
                let outcome = mcp.refresh(&ServerName::new(server.as_str())).await;
                {
                    let mut st = self.0.state.lock();
                    match outcome {
                        Ok(n) => {
                            st.banner = Some(format!(
                                "{server} now lists {n} tool{}",
                                if n == 1 { "" } else { "s" }
                            ))
                        }
                        Err(e) => st.error = Some(e.to_string()),
                    }
                }
                self.0.refresh(cx.tui.clone());
                PaneOutcome::Handled
            }
            Action::Copy(text) => {
                cx.tui.copy(&text).await;
                PaneOutcome::Handled
            }
            Action::Ignored => PaneOutcome::Ignored,
        }
    }
}

#[async_trait::async_trait]
impl Pane for PanelPane {
    fn render(&self, cx: &mut RenderCx<'_>) {
        let mut st = self.state.lock();
        let area = cx.area;
        st.height = area.height.saturating_sub(0);
        let theme = *cx.theme();
        let view = crate::view::lines(&st, area.width, &theme, cx.view.is_focused);
        // Reveal: the selected item's first line stays inside the viewport, however the last
        // refresh moved it.
        if let Some(&line) = view.item_lines.get(st.cursor) {
            let h = area.height.max(1) as usize;
            if line < st.scroll {
                st.scroll = line;
            } else if line >= st.scroll + h {
                st.scroll = line + 1 - h;
            }
        }
        let top = st
            .scroll
            .min(view.lines.len().saturating_sub(area.height as usize));
        let wanted = if st.open {
            view.lines.len().max(1) as u16
        } else {
            0
        };
        drop(st);
        cx.report_aux_rows(wanted);
        for hit in &view.hits {
            let Some(line) = hit.line.checked_sub(top) else {
                continue;
            };
            if (line as u16) >= area.height {
                continue;
            }
            cx.hit(
                ratatui::layout::Rect {
                    x: area.x + hit.x.min(area.width),
                    y: area.y + line as u16,
                    width: hit.width.min(area.width.saturating_sub(hit.x)),
                    height: 1,
                },
                bough_plugin_tui_shell::pane::HitId::new(hit.id.clone()),
            );
        }
        cx.frame
            .render_widget(Paragraph::new(view.lines).scroll((top as u16, 0)), area);
    }

    async fn handle(&self, _ev: PaneEvent, _cx: PaneCx) -> PaneOutcome {
        // The pane is only ever held as `Arc<dyn Pane>`; acting needs an owned `Arc<Self>`, so
        // the real body lives on `PanelPaneArc` (the `tui-search` precedent).
        PaneOutcome::Ignored
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("[ ] or 1-3", "switch tab"),
            ("↑/↓", "select a row"),
            ("enter/click", "open a row's detail"),
            (
                "x",
                "toggle a row on/off (config) · clear an override (model)",
            ),
            ("s", "sweep a collector now"),
            ("r", "re-list a server's tools · refresh"),
            ("R", "the dump renderer's output, verbatim"),
            ("esc", "close"),
        ]
    }
}

#[async_trait::async_trait]
impl Pane for PanelPaneArc {
    fn render(&self, cx: &mut RenderCx<'_>) {
        self.0.render(cx)
    }

    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        match ev {
            PaneEvent::Key(key) => {
                let action = {
                    let mut st = self.0.state.lock();
                    on_key(key, &mut st)
                };
                self.perform(action, &cx).await
            }
            PaneEvent::Click { hit, .. } => {
                let Some(hit) = hit else {
                    return PaneOutcome::Ignored;
                };
                let id = hit.as_str();
                if let Some(title) = id.strip_prefix("panel:tab:") {
                    let tab = Tab::ALL.iter().copied().find(|t| t.title() == title);
                    if let Some(tab) = tab {
                        self.0.state.lock().switch(tab);
                        self.0.refresh(cx.tui.clone());
                        cx.tui.redraw();
                        return PaneOutcome::Handled;
                    }
                    return PaneOutcome::Ignored;
                }
                if let Some(key) = id.strip_prefix("panel:item:") {
                    let mut st = self.0.state.lock();
                    // A click ACTS (the TUI brief, D1): it opens the row's detail. The cursor
                    // follows so the keys continue from what was touched.
                    if let Some(at) = st
                        .items()
                        .iter()
                        .position(|i| st.key_of(i).as_deref() == Some(key))
                    {
                        st.cursor = at;
                    }
                    if !st.expanded.remove(key) {
                        st.expanded.insert(key.to_string());
                    }
                    drop(st);
                    cx.tui.redraw();
                    return PaneOutcome::Handled;
                }
                PaneOutcome::Ignored
            }
            PaneEvent::Scroll { delta } => {
                let painted = {
                    let st = self.0.state.lock();
                    let theme = bough_plugin_tui_shell::theme::Theme::of(
                        bough_plugin_tui_shell::theme::ThemeName::Dark,
                    );
                    crate::view::lines(&st, cx.tui.size().width, &theme, false)
                        .lines
                        .len()
                };
                self.0.state.lock().scroll_by(i32::from(delta), painted);
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            PaneEvent::FocusChanged(true) => {
                if self.0.state.lock().open {
                    self.0.refresh(cx.tui.clone());
                }
                PaneOutcome::Ignored
            }
            PaneEvent::Tick => {
                if self.0.state.lock().due(cx.at, self.0.cfg.refresh_ms) {
                    self.0.refresh(cx.tui.clone());
                }
                PaneOutcome::Ignored
            }
            _ => PaneOutcome::Ignored,
        }
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        self.0.key_hints()
    }
}

/// The pane's listeners, registered from `apply` as effects of THIS row (never the launcher:
/// a row disabled by patch must take its listeners with it, the M15 rule).
pub async fn register_listeners(
    ctx: &bough_kernel::Context,
    pane: Arc<PanelPane>,
    tui: TuiHandle,
) -> Result<(), bough_kernel::PluginError> {
    // The reload banner: the same line the log gets, verbatim.
    let (p, t) = (pane.clone(), tui.clone());
    ctx.on::<bough_kernel::ConfigReloadEvent, _, _>(move |reload| {
        let (p, t) = (p.clone(), t.clone());
        async move {
            {
                let mut st = p.state.lock();
                st.banner = Some(reload.line());
            }
            if p.state.lock().open {
                p.refresh(t.clone());
            }
            t.redraw();
        }
    })
    .await?;
    // A successful update moves the fingerprint: invalidate whatever the panel holds.
    let (p, t) = (pane.clone(), tui.clone());
    ctx.on::<bough_kernel::event::ConfigUpdated, _, _>(move |_fp| {
        let (p, t) = (p.clone(), t.clone());
        async move {
            if p.state.lock().open {
                p.refresh(t.clone());
            }
        }
    })
    .await?;
    // Parked dependents are REPORTED, not pre-guessed (§0.5's D12): one line per row.
    let (p, t) = (pane.clone(), tui.clone());
    ctx.on::<bough_kernel::event::RowsUnresolved, _, _>(move |rows| {
        let (p, t) = (p.clone(), t.clone());
        async move {
            {
                let mut st = p.state.lock();
                st.unresolved = rows
                    .iter()
                    .map(|r| {
                        let what = if r.unmet.is_empty() {
                            r.error
                                .clone()
                                .unwrap_or_else(|| format!("{:?}", r.state).to_lowercase())
                        } else {
                            format!("waiting on {}", r.unmet.join(", "))
                        };
                        format!("{} is enabled but not active: {}", r.id, what)
                    })
                    .collect();
            }
            t.redraw();
        }
    })
    .await?;
    // A server appearing or going away is a reason to re-read; health changes are not signalled
    // (registration-only, by design) — the tick covers those.
    let (p, t) = (pane, tui);
    ctx.on::<bough_plugin_mcp::McpServersChanged, _, _>(move |_change| {
        let (p, t) = (p.clone(), t.clone());
        async move {
            if p.state.lock().open {
                p.refresh(t.clone());
            }
        }
    })
    .await?;
    Ok(())
}
