//! Invariant (P5-D7): a fork/split/bud point is RESOLVED, never clipped and never waited on. §3
//! refuses a fork whose prefix ends inside an open wake; §4 says the parent never pauses. So the
//! resolver walks DOWN to the last seq outside an open wake and the op reports the seq it used.
//! An EXPLICIT `at_seq` inside an open wake is an error, not a silent adjustment.

use bough_plugin_ledger::{Seq, Step};

/// The two step types this module reads. The read filter and the pure walker are kept next to
/// each other so they cannot drift apart (`agent-loop`'s `REPAIR_KINDS` is the precedent), and
/// filtering the query is what keeps a chain readable when some OTHER row's step type has been
/// un-registered by a patch — an unfiltered whole-chain read fails with `UnknownStepTypeOnRead`
/// and takes every split, bud and fork on that lane down with it (D-WP8-5).
pub const WAKE_KINDS: [&str; 2] = ["wake/start", "wake/end"];

/// PURE: the last seq outside an open wake. `head` is the trajectory's true head seq — it is
/// passed in rather than read off the chain because the chain is FILTERED to [`WAKE_KINDS`] and
/// its last row is not the trajectory's last row.
pub fn resolve_point(head: Seq, steps_desc: &[Step]) -> Option<Seq> {
    if head.0 == 0 {
        return None;
    }
    let mut asc: Vec<&Step> = steps_desc.iter().collect();
    asc.sort_by_key(|s| s.seq);
    // Walk DOWN from the head to the first seq that is not inside an open wake. The chain is
    // short-circuited by construction: an open wake is a trailing suffix, so this stops at the
    // `wake/start` boundary at the latest.
    let mut at = head;
    loop {
        if !inside_open_wake_asc(&asc, at) {
            return Some(at);
        }
        if at.0 <= 1 {
            return None;
        }
        at = Seq(at.0 - 1);
    }
}

/// PURE: whether `at` lies inside a wake that has a `wake/start` and no `wake/end`.
pub fn inside_open_wake(steps_desc: &[Step], at: Seq) -> bool {
    let mut asc: Vec<&Step> = steps_desc.iter().collect();
    asc.sort_by_key(|s| s.seq);
    inside_open_wake_asc(&asc, at)
}

/// The same test over a chain already sorted ascending. `ledger-memory`'s `open_wake_at` is the
/// twin of this; both pair `wake/start` with `wake/end` by wake id and ignore everything else.
fn inside_open_wake_asc(asc: &[&Step], at: Seq) -> bool {
    let mut open: Vec<&str> = Vec::new();
    for s in asc.iter().filter(|s| s.seq <= at) {
        match s.kind.as_str() {
            "wake/start" => open.push(s.wake.as_str()),
            "wake/end" => open.retain(|w| *w != s.wake.as_str()),
            _ => {}
        }
    }
    !open.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Class, StepType, TrajId, WakeId};
    use std::sync::Arc;

    fn step(seq: u64, kind: &str, wake: &str) -> Step {
        Step {
            id: bough_plugin_ledger::StepId::new(format!("s{seq}")),
            traj: TrajId::new("lane/sol"),
            seq: Seq(seq),
            at: chrono::Utc::now(),
            wake: WakeId::new(wake),
            kind: StepType::new(kind),
            class: Class::Thought,
            body: Arc::new(serde_json::json!({})),
            cites: Arc::new(vec![]),
            refs: Arc::new(Default::default()),
            ignorable: false,
        }
    }

    /// A chain newest-first, as a `StepQuery { order: SeqDesc }` returns it.
    fn desc(mut v: Vec<Step>) -> Vec<Step> {
        v.sort_by_key(|s| std::cmp::Reverse(s.seq));
        v
    }

    #[test]
    fn the_head_is_the_fork_point_when_no_wake_is_open() {
        let chain = desc(vec![
            step(1, "wake/start", "w1"),
            step(2, "thought/text", "w1"),
            step(3, "wake/end", "w1"),
        ]);
        assert_eq!(resolve_point(Seq(3), &chain), Some(Seq(3)));
        assert!(!inside_open_wake(&chain, Seq(3)));
        assert!(inside_open_wake(&chain, Seq(2)));
    }

    #[test]
    fn an_open_trailing_wake_moves_the_point_below_it() {
        let chain = desc(vec![
            step(1, "wake/start", "w1"),
            step(2, "wake/end", "w1"),
            step(3, "wake/start", "w2"),
            step(4, "thought/text", "w2"),
        ]);
        // 4 and 3 are inside the still-open `w2`; 2 is the last legal point, and the op reports
        // it rather than pausing the parent or clipping silently.
        assert_eq!(resolve_point(Seq(4), &chain), Some(Seq(2)));
        assert!(inside_open_wake(&chain, Seq(4)));
        assert!(!inside_open_wake(&chain, Seq(2)));
        // A chain that is nothing BUT an open wake has no resolvable point at all.
        let all_open = desc(vec![
            step(1, "wake/start", "w1"),
            step(2, "thought/text", "w1"),
        ]);
        assert_eq!(resolve_point(Seq(2), &all_open), None);
        assert_eq!(resolve_point(Seq(0), &[]), None);
    }

    #[test]
    fn a_bud_point_in_the_past_is_taken_as_given() {
        let chain = desc(vec![
            step(1, "wake/start", "w1"),
            step(2, "thought/text", "w1"),
            step(3, "wake/end", "w1"),
            step(4, "wake/start", "w2"),
        ]);
        // The caller named seq 3. It is outside every open wake, so it stands EXACTLY as named —
        // the resolver's own answer would have been 3 too, but the point is that a past seq is
        // never moved up to the head.
        assert!(!inside_open_wake(&chain, Seq(3)));
        assert_eq!(resolve_point(Seq(4), &chain), Some(Seq(3)));
        // A seq in the MIDDLE of a wake is inside it, whether or not that wake later ended: the
        // prefix ending there ends mid-wake, which is what §3 refuses. The ledger's own `fork`
        // uses the same rule, so a bud this module accepts is a bud the store accepts.
        assert!(inside_open_wake(&chain, Seq(2)));
    }
}
