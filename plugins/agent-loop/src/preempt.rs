//! Invariant (§5): checkpoint-and-answer. An Andrey message never waits for a running wake; the
//! answer wake starts immediately and the interrupted wake gets EXACTLY ONE grace step to jot.
//! A jot ALWAYS exists (P2-D14): if the grace step fails or times out, the loop writes a
//! synthetic one from the wake's last thought steps, so the promise "the next wake resumes" never
//! depends on a model call succeeding.

use bough_plugin_agents::vocabulary::WakeJot;
use bough_plugin_agents::Message;
use bough_plugin_ledger::{Step, WakeId};

/// What an arriving message does to the running wake.
#[derive(Clone, Debug, PartialEq)]
pub enum Preemption {
    /// The running wake is not an answer wake: start the answer wake NOW, concurrently, and give
    /// the interrupted wake exactly ONE grace step to jot.
    Checkpoint { answer: WakeId },
    /// An answer wake is running and has NOT streamed a token: the message JOINS it. The
    /// in-flight request is cancelled, `step/end { outcome: restarted }` is appended, and the
    /// same wake starts a new step with both messages claimed (P2-D15).
    Join { wake: WakeId },
    /// An answer wake has already streamed a token: the message queues as the next wake's first
    /// mail (`next-wake`, wake: true).
    Queue,
}

/// The state of the wake an arriving message meets.
#[derive(Clone, Debug, PartialEq)]
pub struct Running<'a> {
    pub wake: &'a WakeId,
    pub is_answer: bool,
    /// "Started responding" means the first reply token has streamed (§5).
    pub streamed: bool,
}

/// The decision, as a pure function of the running wake's state.
///
/// `None` means "nothing to preempt": either the message is not Andrey's (only his message
/// preempts — everything else queues through the ordinary urgency rules) or no wake is running,
/// in which case the driver simply starts one.
pub fn decide(
    msg: &Message,
    running: Option<Running<'_>>,
    next_wake: WakeId,
) -> Option<Preemption> {
    if !msg.is_andrey() {
        return None;
    }
    let running = running?;
    Some(match (running.is_answer, running.streamed) {
        (false, _) => Preemption::Checkpoint { answer: next_wake },
        (true, false) => Preemption::Join {
            wake: running.wake.clone(),
        },
        (true, true) => Preemption::Queue,
    })
}

/// How many trailing thought steps a synthetic jot summarises. A protocol constant, not a
/// tunable: it bounds a body the loop writes for itself, and no deployment varies it (§0.2).
const SYNTHETIC_JOT_STEPS: usize = 3;

/// The synthetic jot, built deterministically from the wake's last thought steps (P2-D14).
///
/// Deterministic on purpose: two runs over the same steps produce the same body, so a repair or a
/// replay never invents a different continuation.
pub fn synthetic_jot(wake: &WakeId, thoughts: &[Step]) -> WakeJot {
    let mut texts: Vec<String> = thoughts
        .iter()
        .filter(|s| matches!(s.kind.as_str(), "thought/text" | "thought/reasoning"))
        .filter_map(|s| {
            s.body
                .get("text")
                .and_then(|v| v.as_str())
                .map(|t| t.trim().to_string())
        })
        .filter(|t| !t.is_empty())
        .collect();
    let tail: Vec<String> = texts
        .drain(texts.len().saturating_sub(SYNTHETIC_JOT_STEPS)..)
        .collect();
    let state = if tail.is_empty() {
        "interrupted before producing anything".to_string()
    } else {
        tail.join("\n")
    };
    WakeJot {
        of_wake: wake.clone(),
        state,
        resume_hint: "resume the interrupted work from the state above".to_string(),
        synthetic: true,
    }
}

/// The instruction the grace step is given. A protocol constant: it is the loop talking to
/// itself, not a deployment-varying value (§0.2).
pub const GRACE_INSTRUCTION: &str =
    "You are being interrupted. In one short paragraph, write down \
where you are and what you would do next, so you can resume later. Do not use tools.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{andrey, ordinary, step, wake_of};

    #[test]
    fn a_message_meeting_a_thought_checkpoints_and_answers() {
        let running = wake_of("w1");
        let next = wake_of("w2");
        let d = decide(
            &andrey("m1", "hi"),
            Some(Running {
                wake: &running,
                is_answer: false,
                streamed: true,
            }),
            next.clone(),
        );
        assert_eq!(d, Some(Preemption::Checkpoint { answer: next }));
    }

    #[test]
    fn a_message_before_the_first_token_joins_and_after_it_queues() {
        let running = wake_of("w1");
        let joined = decide(
            &andrey("m1", "hi"),
            Some(Running {
                wake: &running,
                is_answer: true,
                streamed: false,
            }),
            wake_of("w2"),
        );
        assert_eq!(
            joined,
            Some(Preemption::Join {
                wake: running.clone()
            })
        );
        let queued = decide(
            &andrey("m1", "hi"),
            Some(Running {
                wake: &running,
                is_answer: true,
                streamed: true,
            }),
            wake_of("w2"),
        );
        assert_eq!(queued, Some(Preemption::Queue));
    }

    #[test]
    fn only_andreys_message_preempts_and_only_a_running_wake_is_preempted() {
        let running = wake_of("w1");
        assert_eq!(
            decide(
                &ordinary("m1", None),
                Some(Running {
                    wake: &running,
                    is_answer: false,
                    streamed: false
                }),
                wake_of("w2")
            ),
            None
        );
        assert_eq!(decide(&andrey("m1", "hi"), None, wake_of("w2")), None);
    }

    #[test]
    fn a_synthetic_jot_is_deterministic_and_always_exists() {
        let w = wake_of("w1");
        let steps = vec![
            step(
                1,
                &w,
                "thought/text",
                serde_json::json!({ "text": "one", "step_index": 0 }),
            ),
            step(2, &w, "tool/call", serde_json::json!({ "call": "c" })),
            step(
                3,
                &w,
                "thought/text",
                serde_json::json!({ "text": "two", "step_index": 1 }),
            ),
        ];
        let jot = synthetic_jot(&w, &steps);
        assert!(jot.synthetic);
        assert_eq!(jot.state, "one\ntwo");
        assert_eq!(jot, synthetic_jot(&w, &steps), "deterministic");
        // Even with nothing to summarise a jot exists, so a continuation always can.
        let empty = synthetic_jot(&w, &[]);
        assert!(empty.synthetic && !empty.state.is_empty());
    }
}
