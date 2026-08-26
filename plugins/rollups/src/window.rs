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
pub fn windows(steps: &[Step], cfg: &WindowCfg) -> Vec<Window> {
    let max_steps = cfg.max_steps.max(1);
    let mut out: Vec<Window> = Vec::new();
    let mut cur: Vec<&Step> = Vec::new();
    for s in steps {
        if let Some(prev) = cur.last() {
            if s.at - prev.at >= cfg.gap {
                out.push(close(&cur, Cut::Gap));
                cur.clear();
            } else if cur.len() >= max_steps {
                out.push(close(&cur, Cut::MaxSteps));
                cur.clear();
            }
        }
        cur.push(s);
    }
    if !cur.is_empty() {
        out.push(close(&cur, Cut::Head));
    }
    // A window thinner than `min_steps` is not worth a model call and is dropped here rather
    // than planned and skipped: the planner's `TooShort` is the defensive second statement of the
    // same rule, for a caller that builds windows by hand.
    out.retain(|w| w.steps.len() >= cfg.min_steps);
    out
}

/// Close the accumulator into a window. Never called with an empty slice.
fn close(cur: &[&Step], cut: Cut) -> Window {
    let first = cur.first().expect("a window is never closed empty");
    let last = cur.last().expect("a window is never closed empty");
    Window {
        from_seq: first.seq,
        to_seq: last.seq,
        from_at: first.at,
        to_at: last.at,
        steps: cur.iter().map(|s| s.id.clone()).collect(),
        cut,
    }
}

/// Step fixtures, shared by every pure-algorithm test in this crate. A window is a function of
/// `seq` and `at` alone, so a fixture step carries nothing else that matters.
#[cfg(test)]
pub(crate) mod fixture {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use bough_plugin_ledger::{Class, Ref, Seq, Step, StepId, StepType, TrajId, WakeId};
    use chrono::{DateTime, TimeZone, Utc};

    /// A fixed clock: determinism is the whole value of these tests.
    pub fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0)
            .single()
            .expect("a valid instant")
    }

    pub fn step(seq: u64, secs: i64) -> Step {
        step_with(seq, secs, "probe/note", &[])
    }

    pub fn step_with(seq: u64, secs: i64, kind: &str, refs: &[&str]) -> Step {
        Step {
            id: StepId::new(format!("s{seq}")),
            traj: TrajId::new("t"),
            seq: Seq(seq),
            at: at(secs),
            wake: WakeId::new("w"),
            kind: StepType::new(kind),
            class: Class::Thought,
            body: Arc::new(serde_json::json!({})),
            cites: Arc::new(vec![]),
            refs: Arc::new(refs.iter().map(Ref::new).collect::<BTreeSet<_>>()),
            ignorable: false,
        }
    }

    /// A run of `n` steps one second apart, starting at seq 1.
    pub fn run(n: u64) -> Vec<Step> {
        (1..=n).map(|i| step(i, i as i64)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{run, step};
    use super::*;

    fn cfg(gap_secs: i64, max_steps: usize, min_steps: usize) -> WindowCfg {
        WindowCfg {
            gap: Duration::seconds(gap_secs),
            max_steps,
            min_steps,
        }
    }

    #[test]
    fn a_gap_longer_than_the_cut_ends_the_window() {
        let steps = vec![step(1, 0), step(2, 1), step(3, 600), step(4, 601)];
        let ws = windows(&steps, &cfg(60, 100, 1));
        assert_eq!(ws.len(), 2, "the 599s gap cuts the run in two");
        assert_eq!(ws[0].cut, Cut::Gap);
        assert_eq!((ws[0].from_seq.0, ws[0].to_seq.0), (1, 2));
        assert_eq!((ws[1].from_seq.0, ws[1].to_seq.0), (3, 4));
        assert_eq!(ws[1].cut, Cut::Head);
    }

    #[test]
    fn max_steps_ends_a_window_with_no_gap() {
        let ws = windows(&run(10), &cfg(3600, 4, 1));
        assert_eq!(ws.len(), 3, "10 steps at 4 per window");
        assert_eq!(ws[0].cut, Cut::MaxSteps);
        assert_eq!(ws[1].cut, Cut::MaxSteps);
        assert_eq!(ws[2].cut, Cut::Head);
        assert_eq!(ws[0].steps.len(), 4);
        assert_eq!(ws[2].steps.len(), 2);
    }

    #[test]
    fn the_last_window_is_cut_head_and_is_never_sealed() {
        let ws = windows(&run(9), &cfg(3600, 4, 1));
        let last = ws.last().expect("a run of 9 yields windows");
        assert_eq!(
            last.cut,
            Cut::Head,
            "the open window is always the last one"
        );
        assert_eq!(
            ws.iter().filter(|w| w.cut == Cut::Head).count(),
            1,
            "exactly one window is open"
        );
        // The planner's half of the statement lives in `plan::tests`; here we only pin that the
        // last window is MARKED open, which is what makes it unsealable.
    }

    #[test]
    fn a_run_shorter_than_min_steps_yields_no_window() {
        assert!(windows(&run(3), &cfg(3600, 100, 5)).is_empty());
        assert!(windows(&[], &cfg(3600, 100, 1)).is_empty());
    }

    #[test]
    fn windows_partition_the_run_with_no_overlap() {
        let steps = vec![
            step(1, 0),
            step(2, 1),
            step(3, 900),
            step(4, 901),
            step(5, 902),
            step(6, 903),
            step(7, 904),
        ];
        let ws = windows(&steps, &cfg(60, 3, 1));
        let covered: Vec<u64> = ws.iter().flat_map(|w| w.from_seq.0..=w.to_seq.0).collect();
        assert_eq!(
            covered,
            (1..=7).collect::<Vec<_>>(),
            "every step lands in exactly one window, in order"
        );
        let ids: Vec<&str> = ws
            .iter()
            .flat_map(|w| w.steps.iter().map(|s| s.as_str()))
            .collect();
        assert_eq!(ids.len(), 7, "no step is duplicated across windows");
        for pair in ws.windows(2) {
            assert!(
                pair[0].to_seq.0 < pair[1].from_seq.0,
                "windows do not overlap"
            );
        }
    }
}
