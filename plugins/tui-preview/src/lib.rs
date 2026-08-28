//! Invariant (§11 "Digging", §5): what this pane renders is what the loop would send. It calls the
//! SAME `ctx.projection` the wake flow calls, with the same request defaults, and paints
//! `Assembled::to_text()` verbatim — the bytes are the loop's by construction, not by imitation.
//!
//! The pane is a CONSUMER (§0.2): it registers no service key, owns no write path, and adds no
//! second way to assemble a projection. Its only I/O is one `assemble` read per refresh.
//!
//! `PreviewAt::Seq` is anchored and `PreviewAt::Head` states its delta on the header rather than
//! claiming an exactness today's seam cannot give it (decision D-C1). What V1 asserts
//! (`crates/bough/tests/preview_bytes.rs`) is the pane's own responsibility: its text IS
//! `assemble`'s text for that `as_of`. It does NOT assert that an anchored preview reproduces a
//! past wake's `projection_digest` — it does not, because some sections read live state rather
//! than the ledger below `as_of` (D-C8, `docs/track-c-merge-notes.md`).

pub mod command;
pub mod delta;
pub mod error;
pub mod invariant;
pub mod pane;
pub mod snapshot;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::{Ledger, LedgerHandle};
use bough_plugin_projection::{Projection, ProjectionHandle};
use bough_plugin_tui_shell::pane::{PaneId, PaneSpec, Slot, SlotSize};
use bough_plugin_tui_shell::Tui;

pub use crate::delta::{added_lines, only_preface, WAKE_PREFACE_KINDS};
pub use crate::error::PreviewError;
pub use crate::pane::{on_key, KeyAction, PreviewPane, PreviewPaneArc, PreviewState};
pub use crate::snapshot::{digest, snapshot, system_prefix, PreviewAt, Snapshot};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-preview";

/// The pane id this row registers under. Fixed, because `/preview` names it.
pub const PANE_ID: &str = "tui.preview";

/// The row's config. Every deployment-varying number is here; nothing is hardcoded in the pane.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewConfig {
    /// Rows the pane asks for when the Aux band has room.
    pub height: u16,
    /// Terminal ROWS below which this pane costs zero (`SlotSize::Responsive`'s `collapse`).
    pub collapse_rows: u16,
    pub min_rows: u16,
    pub max_rows: u16,
    /// Debounce on `ledger/step` before re-assembling. Assembly is deterministic but not free.
    pub refresh_ms: u64,
    /// Hard cap on rendered characters, so a 160k-token projection cannot stall a frame.
    pub max_chars: usize,
}

/// The row.
pub struct PreviewPlugin;

#[async_trait::async_trait]
impl Plugin for PreviewPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = PreviewConfig;

    fn inject() -> Inject {
        Inject::required(["tui", "projection", "ledger"])
            .union(&Inject::optional(["agents", "commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        if cfg.height == 0 {
            return reject("height must be > 0; a zero-cell pane can show no line".to_string());
        }
        if cfg.max_chars == 0 {
            return reject(
                "max_chars must be > 0; a preview clipped to nothing shows nothing".to_string(),
            );
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
        let projection = ctx
            .get::<Projection>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let tui = ctx.get::<Tui>().map_err(|e| PluginError::new(entry, e))?;

        // The recorded frame is per-process and this row owns it: unloading forgets what it drew.
        ctx.effect(|e| async move {
            e.defer_sync(invariant::forget);
            Ok(())
        })
        .await?;

        let pane = Arc::new(
            PreviewPane::new(Arc::clone(&cfg))
                .with_seams(
                    ProjectionHandle(projection.0.clone()),
                    LedgerHandle(ledger.0.clone()),
                )
                .with_ctx(ctx.clone()),
        );
        crate::command::register(&ctx, Arc::clone(&pane)).await?;
        // A REGISTRATION IS AN EFFECT: `register_pane` returns the disposer, and unloading this
        // row must leave no pane, no listener and no binding behind.
        tui.register_pane(
            &ctx,
            PaneSpec {
                id: PaneId::new(PANE_ID),
                slot: Slot::Aux,
                order: 10,
                size: SlotSize::Responsive {
                    collapse: cfg.collapse_rows,
                    preferred: cfg.height,
                    min: cfg.min_rows,
                    max: cfg.max_rows,
                },
                title: "preview".into(),
                focusable: true,
                pane: Arc::new(crate::pane::PreviewPaneArc(pane)),
            },
        )
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(PreviewPlugin);
