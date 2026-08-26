//! Invariant (§5): checkpoint-and-answer. An Andrey message never waits for a running wake; the
//! answer wake starts immediately and the interrupted wake gets EXACTLY ONE grace step to jot.
//! A jot ALWAYS exists (P2-D14): if the grace step fails or times out, the loop writes a
//! synthetic one from the wake's last thought steps, so the promise "the next wake resumes" never
//! depends on a model call succeeding.

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

/// The decision, as a pure function of the running wake's state. WP-4.
pub fn decide(
    _msg: &Message,
    _running: Option<(
        &WakeId,
        bool, /* is answer wake */
        bool, /* streamed a token */
    )>,
    _next_wake: WakeId,
) -> Option<Preemption> {
    todo!("WP-4: the three cases of §5, and None when nothing is running")
}

/// The synthetic jot, built deterministically from the wake's last thought steps. WP-4.
pub fn synthetic_jot(
    _wake: &WakeId,
    _thoughts: &[Step],
) -> bough_plugin_agents::vocabulary::WakeJot {
    todo!("WP-4: a deterministic state + resume_hint, synthetic: true")
}
