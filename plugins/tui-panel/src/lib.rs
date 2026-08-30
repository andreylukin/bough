//! Invariant (§11 "The panel", §0.5): what this pane shows IS the running composition — it reads
//! `kernel.composition()` and `kernel.rows_snapshot()`, the same two truths `--dump-config` and
//! the boot report read, and its raw mode prints `bough_kernel::render` verbatim, so the panel
//! is a second CONSUMER of `Composition` and never a second formatter of the dump (Decision D9).
//!
//! The pane is a CONSUMER (§0.2): it registers no service key and owns ONE write path — the ui
//! patch layer file — which the launcher's watch applies exactly as it applies a human's edit.
//! It never calls `kernel.update_tree`: a toggle that bypassed the layer stack would show a tree
//! no dump could explain.

pub mod command;
pub mod data;
pub mod invariant;
pub mod pane;
pub mod state;
pub mod store;
pub mod view;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::Ledger;
use bough_plugin_llm::Llm;
use bough_plugin_mcp::Mcp;
use bough_plugin_schedule::Schedule;
use bough_plugin_tui_shell::events::TuiKeyEvent;
use bough_plugin_tui_shell::pane::{PaneId, PaneSpec, Slot, SlotSize};
use bough_plugin_tui_shell::Tui;

pub use crate::pane::{PanelPane, PanelPaneArc};
pub use crate::state::{Action, PanelState, Tab};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-panel";

/// The pane id this row registers under. Fixed, because `/config` names it.
pub const PANE_ID: &str = "tui.panel";

/// The row's config. Every deployment-varying number is here; nothing is hardcoded in the pane.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PanelConfig {
    /// Rows the pane may take when open (`SlotSize::Responsive`'s `preferred`); closed it
    /// reports zero and costs nothing.
    pub height: u16,
    /// Terminal ROWS below which the pane costs zero even open.
    pub collapse_rows: u16,
    pub min_rows: u16,
    pub max_rows: u16,
    /// Floor between refreshes on ticks and event bursts. A reload event refreshes regardless.
    pub refresh_ms: u64,
}

/// The row.
pub struct PanelPlugin;

#[async_trait::async_trait]
impl Plugin for PanelPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = PanelConfig;

    fn inject() -> Inject {
        Inject::required(["tui"]).union(&Inject::optional([
            "commands", "mcp", "schedule", "ledger", "llm",
        ]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        if cfg.height == 0 {
            return reject("height must be > 0; a zero-cell panel can show no line".to_string());
        }
        if cfg.min_rows > cfg.max_rows {
            return reject(format!(
                "min_rows ({}) must not exceed max_rows ({})",
                cfg.min_rows, cfg.max_rows
            ));
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let tui = ctx
            .get::<Tui>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let err = |e| PluginError::new(entry.clone(), e);
        let mcp = ctx.try_get::<Mcp>().map_err(err)?.map(|h| (*h).clone());
        let err = |e| PluginError::new(entry.clone(), e);
        let schedule = ctx
            .try_get::<Schedule>()
            .map_err(err)?
            .map(|h| (*h).clone());
        let err = |e| PluginError::new(entry.clone(), e);
        let ledger = ctx.try_get::<Ledger>().map_err(err)?.map(|h| (*h).clone());
        let err = |e| PluginError::new(entry.clone(), e);
        let llm = ctx.try_get::<Llm>().map_err(err)?.map(|h| (*h).clone());

        // The recorded write is per-process and this row owns it: unloading forgets it.
        ctx.effect(|e| async move {
            e.defer_sync(invariant::forget);
            Ok(())
        })
        .await?;

        let pane = Arc::new(
            PanelPane::new(Arc::clone(&cfg))
                .with_ctx(ctx.clone())
                .with_seams(mcp, schedule, ledger, llm),
        );

        crate::command::register(&ctx, Arc::clone(&pane)).await?;
        crate::pane::register_listeners(&ctx, Arc::clone(&pane), (*tui).clone()).await?;

        // `^t` without touching the shell's keymap: the `tui/key` waterfall (P3-D18). Consuming
        // sets `handled`; `next` still runs so the chain completes for every listener behind us.
        let (p, t) = (Arc::clone(&pane), (*tui).clone());
        ctx.on_waterfall::<TuiKeyEvent, _, _>(move |mut dispatch, next| {
            let (p, t) = (p.clone(), t.clone());
            async move {
                let is_toggle = matches!(dispatch.key.code, crossterm::event::KeyCode::Char('t'))
                    && dispatch
                        .key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL);
                if is_toggle && !dispatch.handled {
                    dispatch.handled = true;
                    let was_open = {
                        let mut st = p.state.lock();
                        let was = st.open;
                        st.open = !was;
                        was
                    };
                    if was_open {
                        t.give_keyboard_to_composer().await;
                    } else {
                        let tab = p.state.lock().tab();
                        p.open(t.clone(), tab);
                        t.focus_pane(PaneId::new(PANE_ID)).await;
                    }
                    t.redraw();
                }
                next.run(dispatch).await
            }
        })
        .await?;

        // A REGISTRATION IS AN EFFECT: `register_pane` returns the disposer, and unloading this
        // row must leave no pane, no command, no listener and no binding behind.
        tui.register_pane(
            &ctx,
            PaneSpec {
                id: PaneId::new(PANE_ID),
                slot: Slot::Aux,
                order: 5,
                size: SlotSize::Responsive {
                    collapse: cfg.collapse_rows,
                    preferred: cfg.height,
                    min: cfg.min_rows,
                    max: cfg.max_rows,
                },
                title: "panel".into(),
                focusable: true,
                pane: Arc::new(PanelPaneArc(pane)),
            },
        )
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(PanelPlugin);
