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

impl Verdict {
    /// The one-character column this verdict draws in. `?` is the honest glyph for "not enough
    /// evidence": it is neither the calm glyph nor the alarm one.
    pub fn glyph(self) -> &'static str {
        match self {
            Verdict::Steady => "\u{2713}",  // ✓
            Verdict::Watch => "\u{00b7}",   // ·
            Verdict::Flagged => "\u{25b2}", // ▲
            Verdict::TooFewSamples => "?",
        }
    }

    /// The word `/help`, the header and a text assertion all read.
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Steady => "steady",
            Verdict::Watch => "watch",
            Verdict::Flagged => "flagged",
            Verdict::TooFewSamples => "too few samples",
        }
    }
}

/// PURE: the row for one agent's signals.
pub fn dash_row(s: &Signals) -> DashRow {
    DashRow {
        agent: s.agent.clone(),
        samples: s.samples,
        thought_cv: s.thought_len.cv,
        tool_entropy: s.tool_entropy,
        top_tools: s.tool_use.clone(),
        claim_rejection: s.claim_rejection.clone(),
        flags: s.flags.clone(),
        verdict: verdict(s),
    }
}

/// PURE: the verdict. `TooFewSamples` wins over everything: with too little evidence there is no
/// honest verdict to give, and turning that into `Steady` is exactly the §16 violation this
/// dashboard exists not to commit.
///
/// Below the floor `drift-watch` raises [`DriftFlag::TooFewSamples`] and suppresses the other
/// flags, so this reads that flag rather than re-deriving a threshold the pane does not own.
/// Above it: no flag is `Steady`, one flag is `Watch`, two or more is `Flagged`.
pub fn verdict(s: &Signals) -> Verdict {
    if s.flags.contains(&DriftFlag::TooFewSamples) {
        return Verdict::TooFewSamples;
    }
    match s.flags.len() {
        0 => Verdict::Steady,
        1 => Verdict::Watch,
        _ => Verdict::Flagged,
    }
}

/// PURE: the claim-rejection cell. [`SignalState::Inactive`] renders as [`crate::render::UNKNOWN`]
/// and NEVER as `0.00`, which would read as "nothing this agent claimed was rejected".
pub fn claim_cell(state: &SignalState) -> String {
    match state {
        SignalState::Inactive { .. } => crate::render::UNKNOWN.to_string(),
        SignalState::Active { value, n } => format!("{value:.2}/{n}"),
    }
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
pub fn arm(
    prev: Option<(AgentName, DateTime<Utc>)>,
    agent: &AgentName,
    now: DateTime<Utc>,
    arm_ms: u64,
) -> ResetStep {
    match prev {
        Some((armed, at))
            if &armed == agent
                && (now - at).num_milliseconds() >= 0
                && (now - at).num_milliseconds() < arm_ms as i64 =>
        {
            ResetStep::Fire
        }
        _ => ResetStep::Arm,
    }
}

/// PURE: the exact command line the pane dispatches for a row's reset.
///
/// THE reachability of §8's one-command reset from the dashboard, spelled once so the test and the
/// pane cannot disagree. It is `drift-watch`'s own `/reset`, not a second reset (D-C3).
pub fn reset_command(agent: &AgentName) -> String {
    format!("/reset {agent}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_drift_watch::Stat;
    use bough_plugin_ledger::{Seq, SeqRange};

    fn at(ms: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(1_700_000_000_000 + ms).expect("a fixed instant")
    }

    fn signals(flags: Vec<DriftFlag>) -> Signals {
        Signals {
            agent: AgentName::new("sol"),
            window: SeqRange {
                from: Seq(1),
                to: Seq(10),
            },
            samples: 20,
            thought_len: Stat {
                n: 10,
                mean: 100.0,
                variance: 4.0,
                cv: 0.02,
                p50: 100.0,
                p95: 110.0,
            },
            tool_use: vec![ToolShare {
                tool: "bash".into(),
                calls: 10,
                share: 1.0,
            }],
            tool_entropy: 0.0,
            claim_rejection: SignalState::Inactive {
                since: "no claim in the window has been decided".into(),
            },
            flags,
        }
    }

    #[test]
    fn a_flagged_signal_is_not_steady() {
        let one = signals(vec![DriftFlag::ToolUseCollapsed]);
        assert_eq!(verdict(&one), Verdict::Watch);
        let two = signals(vec![
            DriftFlag::ToolUseCollapsed,
            DriftFlag::ThoughtLengthUnstable,
        ]);
        assert_eq!(verdict(&two), Verdict::Flagged);
        assert_ne!(verdict(&one), Verdict::Steady);
        assert_ne!(verdict(&two), Verdict::Steady);
        assert_eq!(verdict(&signals(vec![])), Verdict::Steady);
        // …and the row carries the same verdict the function gives.
        assert_eq!(dash_row(&two).verdict, Verdict::Flagged);
    }

    #[test]
    fn too_few_samples_is_its_own_verdict_not_steady() {
        // The whole §16 rule in one assertion: thin evidence is NOT a calm glyph, and it is not
        // an alarm either.
        let v = verdict(&signals(vec![DriftFlag::TooFewSamples]));
        assert_eq!(v, Verdict::TooFewSamples);
        assert_ne!(v, Verdict::Steady);
        assert_ne!(v, Verdict::Flagged);
        assert_ne!(v.glyph(), Verdict::Steady.glyph());
        // TooFewSamples wins even when it arrives beside another flag.
        assert_eq!(
            verdict(&signals(vec![
                DriftFlag::TooFewSamples,
                DriftFlag::ToolUseCollapsed
            ])),
            Verdict::TooFewSamples
        );
    }

    #[test]
    fn an_inactive_claim_signal_renders_as_unknown() {
        let inactive = SignalState::Inactive {
            since: "no claim in the window has been decided".into(),
        };
        assert_eq!(claim_cell(&inactive), crate::render::UNKNOWN);
        assert_ne!(claim_cell(&inactive), "0.00");
        assert_eq!(
            claim_cell(&SignalState::Active { value: 0.0, n: 7 }),
            "0.00/7"
        );
    }

    #[test]
    fn the_first_r_arms_and_the_second_fires() {
        let sol = AgentName::new("sol");
        assert_eq!(arm(None, &sol, at(0), 3_000), ResetStep::Arm);
        assert_eq!(
            arm(Some((sol.clone(), at(0))), &sol, at(500), 3_000),
            ResetStep::Fire
        );
    }

    #[test]
    fn an_arm_expires_after_arm_ms() {
        let sol = AgentName::new("sol");
        assert_eq!(
            arm(Some((sol.clone(), at(0))), &sol, at(3_000), 3_000),
            ResetStep::Arm
        );
        assert_eq!(
            arm(Some((sol.clone(), at(0))), &sol, at(9_999), 3_000),
            ResetStep::Arm
        );
    }

    #[test]
    fn arming_a_different_agent_replaces_the_arm() {
        let sol = AgentName::new("sol");
        let terra = AgentName::new("terra");
        // `r` on sol, then `r` on terra: terra is ARMED, not RESET. An arm that fired on the row
        // the cursor happened to land on would rebuild the wrong agent's identity.
        assert_eq!(
            arm(Some((sol, at(0))), &terra, at(10), 3_000),
            ResetStep::Arm
        );
    }

    #[test]
    fn the_reset_command_is_drift_watchs_own() {
        assert_eq!(reset_command(&AgentName::new("sol")), "/reset sol");
    }
}
