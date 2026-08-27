//! Invariant: a rate over ZERO decisions is not a number. §8's claim-rejection signal stays
//! Inactive when the window holds no decided claim, because "0% rejected" and "nothing decided"
//! are different facts and only one of them is a drift signal.

use bough_plugin_ledger::Step;

/// The step kind an acceptance is.
pub const CLAIM_ACCEPTED: &str = "claim/accepted";
/// The step kind a rejection is.
pub const CLAIM_REJECTED: &str = "claim/rejected";

/// A rejection rate in `0.0..=1.0`, with the counts that produced it so a caller can render both.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rate {
    pub rejected: usize,
    pub decided: usize,
    pub rate: f64,
}

/// PURE over the steps it is handed: rejected / decided, where an EDIT counts as an acceptance.
/// `None` when nothing in the window was decided.
///
/// An edit is `claim/accepted { edited: true }` — the same step type, because an edit IS an
/// acceptance of a claim Andrey rewrote rather than a third decision. Counting it as a rejection
/// would report drift every time a wording was tightened.
pub fn rejection_rate(steps: &[Step]) -> Option<Rate> {
    let mut rejected = 0usize;
    let mut accepted = 0usize;
    for s in steps {
        match s.kind.as_str() {
            CLAIM_REJECTED => rejected += 1,
            CLAIM_ACCEPTED => accepted += 1,
            _ => {}
        }
    }
    let decided = rejected + accepted;
    if decided == 0 {
        return None;
    }
    Some(Rate {
        rejected,
        decided,
        rate: rejected as f64 / decided as f64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Class, Ref, Seq, StepId, StepType, TrajId, WakeId};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    fn step(seq: u64, kind: &str, body: serde_json::Value) -> Step {
        Step {
            id: StepId::new(format!("s{seq}")),
            traj: TrajId::new("t1"),
            seq: Seq(seq),
            at: chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("a fixed instant"),
            wake: WakeId::new("w1"),
            kind: StepType::new(kind),
            class: Class::Thought,
            body: Arc::new(body),
            cites: Arc::new(Vec::new()),
            refs: Arc::new(BTreeSet::<Ref>::new()),
            ignorable: false,
        }
    }

    fn accepted(seq: u64, edited: bool) -> Step {
        step(
            seq,
            CLAIM_ACCEPTED,
            serde_json::json!({ "claim": format!("c{seq}"), "proposal": "p", "edited": edited }),
        )
    }

    fn rejected(seq: u64) -> Step {
        step(
            seq,
            CLAIM_REJECTED,
            serde_json::json!({ "claim": format!("c{seq}"), "proposal": "p", "reason": "no" }),
        )
    }

    fn proposed(seq: u64) -> Step {
        step(
            seq,
            "claim/proposed",
            serde_json::json!({ "claim": format!("c{seq}"), "kind": "other", "title": "t", "body": "b" }),
        )
    }

    #[test]
    fn the_rate_is_rejected_over_decided() {
        let r = rejection_rate(&[
            proposed(1),
            rejected(2),
            proposed(3),
            accepted(4, false),
            proposed(5),
            rejected(6),
            accepted(7, false),
        ])
        .expect("four claims were decided");
        assert_eq!((r.rejected, r.decided), (2, 4));
        assert!((r.rate - 0.5).abs() < 1e-9, "{r:?}");

        // The DENOMINATOR is decisions, not proposals: three open claims do not dilute it.
        let all_rejected =
            rejection_rate(&[proposed(1), proposed(2), proposed(3), rejected(4)]).expect("one");
        assert_eq!((all_rejected.rejected, all_rejected.decided), (1, 1));
        assert!((all_rejected.rate - 1.0).abs() < 1e-9, "{all_rejected:?}");
    }

    #[test]
    fn an_undecided_window_is_inactive() {
        // Proposals alone are not a rate: nothing has been decided.
        assert_eq!(rejection_rate(&[proposed(1), proposed(2)]), None);
        // Nor is an empty window.
        assert_eq!(rejection_rate(&[]), None);
        // And a window of unrelated steps is not a 0% rejection rate.
        assert_eq!(
            rejection_rate(&[step(1, "thought/text", serde_json::json!({ "text": "x" }))]),
            None
        );
    }

    #[test]
    fn edits_count_as_acceptances() {
        // Two edits and one rejection: a third rejected, not all three.
        let r = rejection_rate(&[accepted(1, true), accepted(2, true), rejected(3)])
            .expect("three decisions");
        assert_eq!((r.rejected, r.decided), (1, 3));
        assert!((r.rate - 1.0 / 3.0).abs() < 1e-9, "{r:?}");

        // An edit alone is a fully ACCEPTED window, not a half-rejected one.
        let edited_only = rejection_rate(&[accepted(1, true)]).expect("one decision");
        assert_eq!(edited_only.rate, 0.0);
        assert_eq!(edited_only.decided, 1);
    }
}
