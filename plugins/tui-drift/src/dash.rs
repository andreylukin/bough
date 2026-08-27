//! Invariant (§16): uncertainty never becomes assertion. `TooFewSamples` is its OWN verdict — not
//! `Steady`, and not `Flagged` — and an inactive signal renders as unknown, never as `0.00`. Every
//! number on this dashboard is `drift-watch`'s; this module only decides how to say it.

use bough_plugin_drift_watch::{DriftFlag, SignalState, Signals, ToolShare};
use bough_plugin_ledger::AgentName;
use chrono::{DateTime, Utc};

/// PURE: one dashboard row, from [`Signals`] alone. No clock, no ledger.
#[derive(Clone, Debug, PartialEq)]
pub struct DashRow {
    pub agent: AgentName,
    pub samples: usize,
    pub thought_cv: f64,
    pub tool_entropy: f64,
    pub top_tools: Vec<ToolShare>,
    pub claim_rejection: SignalState,
    pub flags: Vec<DriftFlag>,
    pub verdict: Verdict,
}

/// What the glyph column says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    Steady,
    Watch,
    Flagged,
    /// NOT `Steady` and not `Flagged`: too little evidence to say either (§16).
    TooFewSamples,
}

/// PURE: the row for one agent's signals.
///
/// WP-3.
pub fn dash_row(s: &Signals) -> DashRow {
    let _ = s;
    todo!("WP-3: project Signals onto DashRow")
}

/// PURE: the verdict. `TooFewSamples` wins over everything: with too little evidence there is no
/// honest verdict to give.
///
/// WP-3.
pub fn verdict(s: &Signals) -> Verdict {
    let _ = s;
    todo!("WP-3: TooFewSamples, else Flagged/Watch/Steady by the flags")
}

/// The two-step arm (decision D-C5). One keystroke rebuilding an agent's identity is not a surface
/// a daily driver should have: `r` arms with a visible notice, a second `r` within `arm_ms` fires.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResetStep {
    Arm,
    Fire,
}

/// PURE: the arm state machine. `Arm` on the first `r`; `Fire` only on a second `r` for the SAME
/// agent within `arm_ms`. Arming a different agent replaces the arm rather than firing it.
///
/// WP-3.
pub fn arm(
    prev: Option<(AgentName, DateTime<Utc>)>,
    agent: &AgentName,
    now: DateTime<Utc>,
    arm_ms: u64,
) -> ResetStep {
    let _ = (prev, agent, now, arm_ms);
    todo!("WP-3: Fire iff same agent and within arm_ms, else Arm")
}

/// PURE: the exact command line the pane dispatches for a row's reset.
///
/// THE reachability of §8's one-command reset from the dashboard, spelled once so the test and the
/// pane cannot disagree.
///
/// WP-3.
pub fn reset_command(agent: &AgentName) -> String {
    let _ = agent;
    todo!("WP-3: format!(\"/reset {agent}\")")
}
