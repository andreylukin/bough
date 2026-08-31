//! Invariant (P5-D7): the fork point is RESOLVED. The parent's head when it is outside an open
//! wake, else the last seq that is. Never pauses, never waits, never clips silently — §4 says the
//! parent never pauses, and §3 refuses a fork whose prefix ends inside an open wake, so the only
//! honest answer is to branch below the open wake and REPORT the seq used.

use bough_plugin_ledger::{Seq, Step};

/// The two step types that open and close a wake. Read by NAME (P3-D11): the wake vocabulary
/// belongs to `agents`, and this crate does not depend on the crate that writes it. They are also
/// exactly the rows the LEDGER's own fork rule looks at, which is what makes the point it returns
/// one the ledger will accept.
pub const WAKE_START: &str = "wake/start";
pub const WAKE_END: &str = "wake/end";

/// PURE: the seq a fork may branch at, given the parent's chain newest-first. `None` when there is
/// no closed prefix at all.
///
/// The parent's head when no wake is open there, else the last seq strictly below the EARLIEST
/// open wake's `wake/start` — the same rule the ledger refuses a fork by, so a point this returns
/// is one `Ledger::fork` accepts. More than one wake can be open at once (a worker's `worker/started`
/// lands in the spawner's chain under the spawner's running wake), so it is the earliest that
/// decides, never merely the head's own.
/// The two step types [`fork_point`] reads, and the filter its caller's query must carry. Kept
/// next to the walker so the read and the rule cannot drift apart (`agent-loop`'s `REPAIR_KINDS`
/// is the precedent). Reading the WHOLE chain instead fails with `UnknownStepTypeOnRead` as soon
/// as any row that declared a step type on that chain is disabled by patch (D-WP8-5).
pub const WAKE_KINDS: [&str; 2] = [WAKE_START, WAKE_END];

/// `head` is the trajectory's true head seq: the chain is FILTERED to [`WAKE_KINDS`], so its last
/// row is not the trajectory's last row.
pub fn fork_point(head: Seq, steps_desc: &[Step]) -> Option<Seq> {
    let mut open: Vec<(&bough_plugin_ledger::WakeId, Seq)> = Vec::new();
    // Ascending, so a `wake/end` can only close a start already seen.
    let mut asc: Vec<&Step> = steps_desc.iter().collect();
    asc.sort_by_key(|s| s.seq.0);
    for s in &asc {
        match s.kind.as_str() {
            WAKE_START => open.push((&s.wake, s.seq)),
            WAKE_END => open.retain(|(w, _)| *w != &s.wake),
            _ => {}
        }
    }
    match open.iter().map(|(_, seq)| *seq).min() {
        // Nothing open: the head is the point.
        None => (head.0 > 0).then_some(head),
        // Something open: the last row strictly below it, and never inside it. Seqs are dense per
        // trajectory, so that row is exactly `first_open - 1`; reading it off the FILTERED chain
        // would answer with the last wake row instead, which can be several steps too low.
        Some(first_open) => (first_open.0 > 1).then(|| Seq(first_open.0 - 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Class, StepId, StepType, TrajId, WakeId};
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    /// One row, newest-first order supplied by the caller.
    fn step(seq: u64, wake: &str, kind: &str) -> Step {
        Step {
            id: StepId::new(format!("s{seq}")),
            traj: TrajId::new("parent"),
            seq: Seq(seq),
            at: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
            wake: WakeId::new(wake),
            kind: StepType::new(kind),
            class: Class::Thought,
            body: Arc::new(serde_json::json!({})),
            cites: Arc::new(Vec::new()),
            refs: Arc::new(BTreeSet::new()),
            ignorable: false,
        }
    }

    /// Newest first, as the query hands them over.
    fn desc(mut v: Vec<Step>) -> Vec<Step> {
        v.sort_by_key(|s| std::cmp::Reverse(s.seq.0));
        v
    }

    #[test]
    fn the_head_is_the_point_when_no_wake_is_open() {
        let steps = desc(vec![
            step(1, "w1", WAKE_START),
            step(2, "w1", "step/end"),
            step(3, "w1", WAKE_END),
        ]);
        assert_eq!(fork_point(Seq(3), &steps), Some(Seq(3)));
    }

    #[test]
    fn an_open_trailing_wake_moves_the_point_below_it() {
        let steps = desc(vec![
            step(1, "w1", WAKE_START),
            step(2, "w1", WAKE_END),
            step(3, "w2", WAKE_START),
            step(4, "w2", "step/start"),
        ]);
        assert_eq!(
            fork_point(Seq(4), &steps),
            Some(Seq(2)),
            "the point is the last seq OUTSIDE the open wake, and the parent never pauses"
        );

        // TWO wakes open at once — a worker's `worker/started` lands in the spawner's chain under
        // its own wake while the spawner's wake is still running. It is the EARLIEST open wake
        // that decides; walking below the head's own would land inside the other, which is what
        // the ledger refuses.
        let two = desc(vec![
            step(1, "w1", WAKE_START),
            step(2, "w1", WAKE_END),
            step(3, "w2", WAKE_START),
            step(4, "w2", "step/start"),
            step(5, "w3", WAKE_START),
            step(6, "w3", "step/start"),
        ]);
        assert_eq!(fork_point(Seq(6), &two), Some(Seq(2)));
    }

    #[test]
    fn an_empty_chain_has_no_point() {
        assert_eq!(fork_point(Seq(0), &[]), None);
        // And a chain that is nothing but one open wake has none either: there is no closed
        // prefix to fork at, and clipping into the open wake is what §3 refuses.
        let only_open = desc(vec![step(1, "w1", WAKE_START), step(2, "w1", "step/start")]);
        assert_eq!(fork_point(Seq(2), &only_open), None);

        // And the chain the caller passes is FILTERED to the wake vocabulary, so the answer must
        // not be read off its last row: here the last legal point is the thought at seq 4, which
        // is not a wake row at all.
        let filtered = desc(vec![
            step(1, "w1", WAKE_START),
            step(2, "w1", WAKE_END),
            step(5, "w2", WAKE_START),
        ]);
        assert_eq!(
            fork_point(Seq(6), &filtered),
            Some(Seq(4)),
            "the last row below the open wake, not the last WAKE row below it"
        );
    }
}
