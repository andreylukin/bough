//! Invariant: episode windows are a PURE function of the covered steps' `at` and `seq` alone
//! (§8: "episode windows cut at time gaps"). `now` is never read here, so the same run of steps
//! cuts the same way today and in a replay a year from now.

use bough_plugin_ledger::{Seq, Step, StepId};
use chrono::{DateTime, Duration, Utc};

/// Why a window ended. [`Cut::Head`] is the last, still-open window and is never sealed.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Cut {
    Gap,
    MaxSteps,
    Head,
}

/// One episode window.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Window {
    pub from_seq: Seq,
    pub to_seq: Seq,
    pub from_at: DateTime<Utc>,
    pub to_at: DateTime<Utc>,
    pub steps: Vec<StepId>,
    pub cut: Cut,
}

/// How a run of steps is cut.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowCfg {
    /// A gap this long or longer between consecutive steps ENDS the window (§8).
    pub gap: Duration,
    pub max_steps: usize,
    pub min_steps: usize,
}

/// Cut a step run into episode windows.
///
/// Total, order-preserving, and a pure function of the steps' `at` and `seq` alone: the windows
/// partition the run with no overlap and no gap, and the last one is always [`Cut::Head`].
pub fn windows(_steps: &[Step], _cfg: &WindowCfg) -> Vec<Window> {
    todo!("WP-1: episode windowing")
}
