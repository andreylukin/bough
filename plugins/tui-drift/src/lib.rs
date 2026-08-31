//! Invariant (§8): the dashboard is a VIEW of `drift-watch`'s per-agent signals and nothing more.
//! It computes no signal of its own, writes nothing, and reaches §8's one-command reset by
//! dispatching `drift-watch`'s own `/reset` through the `commands` seam (decision D-C3), so there
//! stays exactly one implementation of a reset.
//!
//! `TooFewSamples` is a verdict of its own: this pane never turns thin evidence into a `Steady`
//! glyph (§16).
//!
pub mod command;
pub mod dash;
pub mod invariant;
pub mod pane;
pub mod render;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_drift_watch::Drift;
use bough_plugin_ledger::{Ledger, LedgerHandle};
use bough_plugin_tui_shell::pane::{PaneId, PaneSpec, Slot, SlotSize};
use bough_plugin_tui_shell::Tui;

pub use crate::dash::{arm, dash_row, reset_command, verdict, DashRow, ResetStep, Verdict};
pub use crate::pane::{DriftPane, DriftState};
pub use crate::render::{bar, line};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-drift";

/// The pane id this row registers under. Fixed, because `/driftboard` names it.
pub const PANE_ID: &str = "tui.drift";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriftPaneConfig {
    pub height: u16,
    pub collapse_rows: u16,
    pub min_rows: u16,
    pub max_rows: u16,
    /// Most agents shown; the rest are a `… N more` line.
    pub agents_shown: usize,
    pub refresh_ms: u64,
    /// Columns the tool-share bar gets.
    pub bar_cols: u16,
    /// Milliseconds the reset stays armed after the first `r` (decision D-C5).
    pub arm_ms: u64,
}

/// The row.
pub struct DriftBoardPlugin;

#[async_trait::async_trait]
impl Plugin for DriftBoardPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = DriftPaneConfig;

    fn inject() -> Inject {
        Inject::required(["tui", "drift", "ledger"])
            .union(&Inject::optional(["agents", "commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        if cfg.height == 0 {
            return reject("height must be > 0; a zero-cell pane can show no agent".to_string());
        }
        if cfg.min_rows == 0 {
            return reject("min_rows must be > 0".to_string());
        }
        if cfg.max_rows < cfg.min_rows {
            return reject(format!(
                "max_rows ({}) must be >= min_rows ({})",
                cfg.max_rows, cfg.min_rows
            ));
        }
        // A dashboard that shows nobody is a pane that renders a header and lies by omission.
        if cfg.agents_shown == 0 {
            return reject("agents_shown must be > 0".to_string());
        }
        // A zero-width bar is a column that says nothing where a share belongs.
        if cfg.bar_cols == 0 {
            return reject("bar_cols must be > 0".to_string());
        }
        // `refresh_ms: 0` polls the ledger on every tick, for every agent.
        if cfg.refresh_ms == 0 {
            return reject(
                "refresh_ms must be > 0; a zero refresh polls on every tick".to_string(),
            );
        }
        // `arm_ms: 0` collapses the two-step arm into a single keystroke that rebuilds an
        // agent's identity (D-C5). That is the surface the arm exists to remove.
        if cfg.arm_ms == 0 {
            return reject(
                "arm_ms must be > 0; a zero arm window makes a single `r` a reset".to_string(),
            );
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let fail = |e: bough_kernel::KernelError| PluginError::new(entry.clone(), e);

        let tui = ctx.get::<Tui>().map_err(fail)?;
        let drift = (*ctx.get::<Drift>().map_err(fail)?).clone();
        let ledger = LedgerHandle(ctx.get::<Ledger>().map_err(fail)?.0.clone());

        let size = SlotSize::Responsive {
            collapse: cfg.collapse_rows,
            preferred: cfg.height,
            min: cfg.min_rows,
            max: cfg.max_rows,
        };
        let pane = DriftPane::new(Arc::clone(&cfg)).with_seams(drift, ledger);
        // A REGISTRATION IS AN EFFECT: `register_pane` returns the disposer, so disabling this row
        // by patch leaves no pane, no listener and no binding behind (§0.2, the swap gate).
        tui.register_pane(
            &ctx,
            PaneSpec {
                id: PaneId::new(PANE_ID),
                slot: Slot::Aux,
                order: 30,
                size,
                title: "drift".into(),
                focusable: true,
                pane: Arc::new(pane),
            },
        )
        .await?;

        command::register(&ctx).await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(DriftBoardPlugin);
