//! Invariant (§11, §17 Phase 8): the timeline is a PURE function of the ledger stream. Every row
//! it shows is a step somebody appended; the order is total and deterministic; and the filters
//! compose as a conjunction of five independent dimensions, so narrowing one can never widen the
//! result.
//!
//! The pane is a CONSUMER of `ledger` (§0.2): no service key, no write path, no wake. Clicking a
//! row is a `FocusRequest` on that step, exactly as `tui-search` focuses a hit.
//!
//! The one impure function in the crate is [`load_rows`], and it is a READ. Everything the pane
//! decides — the order, the truncation, the filter, the rendered line, the hit id — is a pure
//! function of rows it already holds and a `now` it is given.

pub mod command;
pub mod error;
pub mod filter;
pub mod invariant;
pub mod order;
pub mod pane;
pub mod render;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::Agents;
use bough_plugin_ledger::{AgentName, Ledger, LedgerHandle, Step, TrajId};
use bough_plugin_tui_shell::pane::{PaneId, PaneSpec, Slot, SlotSize};
use bough_plugin_tui_shell::Tui;

pub use crate::error::{FilterError, TimelineError};
pub use crate::filter::{parse_filter, render_filter, Filter};
pub use crate::order::timeline;
pub use crate::pane::{TimelinePane, TimelineState};
pub use crate::render::{hit_of, line};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-timeline";

/// The pane id this row registers under. Fixed, because `/timeline` names it.
pub const PANE_ID: &str = "tui.timeline";

/// One row of the timeline: a step, and whose it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub agent: AgentName,
    pub traj: TrajId,
    pub step: Step,
}

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimelineConfig {
    pub height: u16,
    pub collapse_rows: u16,
    pub min_rows: u16,
    pub max_rows: u16,
    /// Newest steps read PER TRAJECTORY before filtering. The read bound.
    pub window: usize,
    /// Rows rendered after filtering. The render bound.
    pub limit: usize,
    pub debounce_ms: u64,
    /// `chrono` format for the time column.
    pub time_format: String,
}

/// What one read returned, and whether it reached its horizon.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loaded {
    pub rows: Vec<Row>,
    /// `true` when at least one trajectory's window was FULL: older steps exist and were NOT read.
    /// The header says so rather than letting an unread step look like one that never happened
    /// (§16).
    pub windowed: bool,
}

/// The ONE read in this crate: the newest [`TimelineConfig::window`] steps of every trajectory,
/// as `Row`s. Nothing here orders, filters by time, or truncates — that is [`timeline`]'s job, and
/// keeping the two apart is what lets the timeline be tested as a pure function.
pub async fn load_rows(
    ledger: &LedgerHandle,
    cfg: &TimelineConfig,
    filter: &Filter,
) -> Result<Loaded, TimelineError> {
    let agents = ledger.0.agents().await?;
    let mut out = Loaded::default();
    for agent in agents {
        // An `agent:` conjunct is applied HERE only as a read economy: `matches` applies it again,
        // so a row that reached the screen was checked by the filter itself either way.
        if !filter.agents.is_empty() && !filter.agents.contains(&agent.name) {
            continue;
        }
        let q = filter.to_query(vec![agent.traj.clone()], cfg.window);
        let steps = ledger.0.steps(&q).await?;
        out.windowed |= steps.len() >= cfg.window;
        for step in steps {
            out.rows.push(Row {
                agent: agent.name.clone(),
                traj: agent.traj.clone(),
                step,
            });
        }
    }
    Ok(out)
}

/// The row.
pub struct TimelinePlugin;

#[async_trait::async_trait]
impl Plugin for TimelinePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = TimelineConfig;

    fn inject() -> Inject {
        Inject::required(["tui", "ledger"]).union(&Inject::optional(["agents", "commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        if cfg.height == 0 {
            return reject("height must be > 0; a zero-cell pane can show no row".to_string());
        }
        if cfg.window == 0 {
            return reject("window must be > 0; a zero-step read can show nothing".to_string());
        }
        if cfg.limit == 0 {
            return reject("limit must be > 0; a zero-row timeline is a blank pane".to_string());
        }
        if cfg.min_rows > cfg.max_rows {
            return reject(format!(
                "min_rows ({}) must not exceed max_rows ({})",
                cfg.min_rows, cfg.max_rows
            ));
        }
        // An unparseable `chrono` format renders as the literal text, silently, on every row.
        // Formatting one known instant is the only way to find that out before boot.
        let probe = chrono::DateTime::<chrono::Utc>::UNIX_EPOCH;
        if std::panic::catch_unwind(|| probe.format(&cfg.time_format).to_string()).is_err() {
            return reject(format!(
                "time_format `{}` is not a chrono format string",
                cfg.time_format
            ));
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let ledger = LedgerHandle(ledger.0.clone());
        let tui = ctx
            .get::<Tui>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let agents = ctx
            .try_get::<Agents>()
            .map_err(|e| PluginError::new(entry, e))?;

        // The recorded frame is per-process and this row owns it: unloading forgets what it drew.
        ctx.effect(|e| async move {
            e.defer_sync(invariant::forget);
            Ok(())
        })
        .await?;

        let pane = Arc::new(
            TimelinePane::new(Arc::clone(&cfg))
                .with_ledger(ledger)
                .with_agents(agents)
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
                order: 20,
                size: SlotSize::Responsive {
                    collapse: cfg.collapse_rows,
                    preferred: cfg.height,
                    min: cfg.min_rows,
                    max: cfg.max_rows,
                },
                title: "timeline".into(),
                focusable: true,
                pane: Arc::new(crate::pane::TimelinePaneArc(pane)),
            },
        )
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(TimelinePlugin);

/// Row builders the crate's own tests and its integration tests share. Public because
/// `tests/filters.rs` and `tests/purity.rs` are separate crates and must build the same rows the
/// unit tests do — a second builder would let the two disagree about what a row is.
#[doc(hidden)]
pub mod testing {
    use super::*;
    use bough_plugin_ledger::{Class, Seq, StepId, StepType, WakeId};
    use chrono::{DateTime, Utc};

    /// The day every test row is stamped on.
    pub const DAY: &str = "2026-08-27";

    /// One row: `at` is `"HH:MM:SS"` on [`DAY`].
    pub fn row(agent: &str, traj: &str, seq: u64, kind: &str, at: &str) -> Row {
        Row {
            agent: AgentName::new(agent),
            traj: TrajId::new(traj),
            step: Step {
                id: StepId::new(format!("{traj}-{seq}")),
                traj: TrajId::new(traj),
                seq: Seq(seq),
                at: instant(at),
                wake: WakeId::new(format!("w-{traj}")),
                kind: StepType::new(kind),
                class: Class::Thought,
                body: Arc::new(serde_json::json!({"text": format!("{kind} #{seq}")})),
                cites: Arc::new(Vec::new()),
                refs: Arc::new(std::collections::BTreeSet::new()),
                ignorable: false,
            },
        }
    }

    /// `"HH:MM:SS"` on [`DAY`], UTC.
    pub fn instant(at: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("{DAY}T{at}Z"))
            .unwrap_or_else(|e| panic!("bad test instant {at:?}: {e}"))
            .with_timezone(&Utc)
    }

    /// A config the pure functions are happy with.
    pub fn config() -> TimelineConfig {
        TimelineConfig {
            height: 12,
            collapse_rows: 24,
            min_rows: 6,
            max_rows: 20,
            window: 200,
            limit: 200,
            debounce_ms: 60,
            time_format: "%H:%M:%S".to_string(),
        }
    }
}
