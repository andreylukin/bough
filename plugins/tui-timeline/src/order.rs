//! Invariant: [`timeline`] is a PURE function of `(rows, filter, limit)`. It reads no clock, no
//! ledger and no state; it owns the total order, the truncation to the newest `limit`, and
//! nothing else. That is what makes "the timeline is a pure function of the ledger stream" a
//! property a test can hold rather than a sentence in a design note.
//!
//! The order is `step.at` ascending, ties broken by `(traj, seq)`. Wall-clock ties across
//! trajectories are common — two agents woken by one event stamp the same instant — so the
//! tiebreak is not a nicety: without it the same input in a different order would render
//! differently, and a reader comparing two screenshots would be reading noise.

use crate::filter::Filter;
use crate::Row;

/// PURE — **the** timeline. Filtered, totally ordered, truncated to the NEWEST `limit` rows, and
/// returned oldest-first.
pub fn timeline(rows: &[Row], f: &Filter, limit: usize) -> Vec<Row> {
    let mut kept: Vec<Row> = rows.iter().filter(|r| f.matches(r)).cloned().collect();
    kept.sort_by(|a, b| {
        a.step
            .at
            .cmp(&b.step.at)
            .then_with(|| a.traj.cmp(&b.traj))
            .then_with(|| a.step.seq.cmp(&b.step.seq))
    });
    // The NEWEST `limit`: the truncation drops the head, never the tail. A timeline that dropped
    // the newest rows would go quiet exactly when something was happening.
    if kept.len() > limit {
        kept.drain(..kept.len() - limit);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::row;

    #[test]
    fn rows_from_two_trajectories_interleave_by_at() {
        let rows = vec![
            row("sol", "t1", 1, "wake/start", "12:00:00"),
            row("sol", "t1", 2, "tool/call", "12:00:30"),
            row("terra", "t2", 1, "wake/start", "12:00:10"),
            row("terra", "t2", 2, "tool/call", "12:00:20"),
        ];
        let out = timeline(&rows, &Filter::default(), 100);
        let seen: Vec<String> = out
            .iter()
            .map(|r| format!("{}:{}", r.agent, r.step.seq.0))
            .collect();
        assert_eq!(seen, ["sol:1", "terra:1", "terra:2", "sol:2"]);
    }

    #[test]
    fn a_tie_on_at_is_broken_by_traj_then_seq() {
        let rows = vec![
            row("terra", "t2", 7, "x", "12:00:00"),
            row("sol", "t1", 9, "x", "12:00:00"),
            row("sol", "t1", 8, "x", "12:00:00"),
        ];
        let out = timeline(&rows, &Filter::default(), 100);
        let seen: Vec<String> = out
            .iter()
            .map(|r| format!("{}/{}", r.traj, r.step.seq.0))
            .collect();
        assert_eq!(seen, ["t1/8", "t1/9", "t2/7"]);
    }

    #[test]
    #[allow(non_snake_case)]
    fn the_limit_keeps_the_NEWEST_rows_and_returns_them_oldest_first() {
        let rows = vec![
            row("sol", "t1", 1, "x", "12:00:00"),
            row("sol", "t1", 2, "x", "12:00:01"),
            row("sol", "t1", 3, "x", "12:00:02"),
            row("sol", "t1", 4, "x", "12:00:03"),
        ];
        let out = timeline(&rows, &Filter::default(), 2);
        let seen: Vec<u64> = out.iter().map(|r| r.step.seq.0).collect();
        assert_eq!(seen, [3, 4], "the newest two, oldest first");
        assert_eq!(timeline(&rows, &Filter::default(), 0), Vec::<Row>::new());
    }

    #[test]
    fn timeline_is_a_pure_function_of_its_input_slice() {
        let rows = vec![
            row("sol", "t1", 1, "wake/start", "12:00:00"),
            row("terra", "t2", 1, "tool/call", "12:00:05"),
        ];
        let a = timeline(&rows, &Filter::default(), 10);
        let b = timeline(&rows, &Filter::default(), 10);
        assert_eq!(a, b);
        // …and it did not touch what it was given.
        assert_eq!(
            rows.iter().map(|r| r.step.seq.0).collect::<Vec<_>>(),
            [1, 1]
        );
    }

    #[test]
    fn the_same_input_in_a_shuffled_order_yields_the_same_output() {
        let rows = vec![
            row("sol", "t1", 1, "x", "12:00:00"),
            row("sol", "t1", 2, "x", "12:00:02"),
            row("terra", "t2", 1, "x", "12:00:01"),
            row("terra", "t2", 2, "x", "12:00:02"),
            row("scout", "t3", 1, "x", "12:00:03"),
        ];
        let want = timeline(&rows, &Filter::default(), 10);
        // Every rotation of the input is the same multiset, so every one must render identically.
        for k in 1..rows.len() {
            let mut shuffled = rows[k..].to_vec();
            shuffled.extend_from_slice(&rows[..k]);
            assert_eq!(
                timeline(&shuffled, &Filter::default(), 10),
                want,
                "shift {k}"
            );
        }
    }
}
